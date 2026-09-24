#!/usr/bin/env python3
"""sudo → Lean 4 protocol-4 emitter.

Reads one emit-request envelope on stdin; writes {"files":[...]} or {"error"}
on stdout. Strict parse: unknown fields/versions/variants are errors.

Lean 4.14, no Mathlib. Generated core is total: loops become `let rec` on a
Nat fuel (for-range/for-in use the remaining-iteration count; while uses 2^32).
Non-total constructs are not lowered to `partial` / `sorry`.
"""
from __future__ import annotations

import json
import sys
from dataclasses import dataclass, field
from typing import Any, Callable, Optional

# ---------------------------------------------------------------------------
# Strict JSON helpers
# ---------------------------------------------------------------------------


class DecErr(Exception):
    pass


def expect_keys(allowed: list[str], obj: Any, ctx: str = "") -> dict[str, Any]:
    if not isinstance(obj, dict):
        raise DecErr(f"{ctx or 'value'}: expected object")
    extra = [k for k in obj if k not in allowed]
    if extra:
        raise DecErr(f"{ctx or 'object'}: unknown fields: {extra}")
    missing = [k for k in allowed if k not in obj]
    if missing:
        raise DecErr(f"{ctx or 'object'}: missing fields: {missing}")
    return obj


def ext_tag(v: Any, ctx: str = "") -> tuple[str, Any]:
    """Serde external tagging: bare string = unit, single-key object = payload."""
    if isinstance(v, str):
        return v, None
    if isinstance(v, dict):
        if len(v) != 1:
            raise DecErr(f"{ctx}: expected single-key object for enum, got keys {list(v)}")
        k, val = next(iter(v.items()))
        return k, val
    raise DecErr(f"{ctx}: expected string or single-key object for enum tag")


def as_str(v: Any, ctx: str = "") -> str:
    if isinstance(v, str):
        return v
    raise DecErr(f"{ctx}: expected string")


def as_bool(v: Any, ctx: str = "") -> bool:
    if isinstance(v, bool):
        return v
    raise DecErr(f"{ctx}: expected bool")


def as_arr(v: Any, ctx: str = "") -> list[Any]:
    if isinstance(v, list):
        return v
    raise DecErr(f"{ctx}: expected array")


def as_i64_str(v: Any, ctx: str = "") -> int:
    if not isinstance(v, str):
        raise DecErr(f"{ctx}: expected i64 decimal string")
    if v == "" or v == "-" or (v[0] == "-" and not v[1:].isdigit()) or (v[0] != "-" and not v.isdigit()):
        raise DecErr(f"{ctx}: invalid i64 string: {v!r}")
    n = int(v)
    if n < -(1 << 63) or n > (1 << 63) - 1:
        raise DecErr(f"{ctx}: i64 out of range: {v}")
    return n


def as_i64_num(v: Any, ctx: str = "") -> int:
    """Plain JSON number → i64 (text scalar arrays)."""
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        raise DecErr(f"{ctx}: expected number for i64")
    if isinstance(v, float):
        if not v.is_integer():
            raise DecErr(f"{ctx}: expected integer number, got {v}")
        n = int(v)
    else:
        n = v
    if n < -(1 << 63) or n > (1 << 63) - 1:
        raise DecErr(f"{ctx}: i64 out of range: {n}")
    return n


def as_line(v: Any, ctx: str = "") -> int:
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        raise DecErr(f"{ctx}: line must be number")
    if isinstance(v, float) and not v.is_integer():
        raise DecErr(f"{ctx}: bad line number: {v}")
    return int(v)


def as_float(v: Any, ctx: str = "") -> float:
    if isinstance(v, str):
        if v == "nan":
            return float("nan")
        if v == "inf":
            return float("inf")
        if v == "-inf":
            return float("-inf")
        raise DecErr(f"{ctx}: unknown float string: {v}")
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        raise DecErr(f"{ctx}: expected float number or nan/inf/-inf string")
    return float(v)


# ---------------------------------------------------------------------------
# IR
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Ty:
    tag: str
    args: tuple[Any, ...] = ()

    @staticmethod
    def int() -> "Ty":
        return Ty("Int")

    @staticmethod
    def float() -> "Ty":
        return Ty("Float")

    @staticmethod
    def bool() -> "Ty":
        return Ty("Bool")

    @staticmethod
    def unit() -> "Ty":
        return Ty("Tuple", ())

    def is_int(self) -> bool:
        return self.tag == "Int"

    def is_float(self) -> bool:
        return self.tag == "Float"

    def is_bool(self) -> bool:
        return self.tag == "Bool"

    def is_list(self) -> bool:
        return self.tag == "List"

    def is_map(self) -> bool:
        return self.tag == "Map"

    def is_set(self) -> bool:
        return self.tag == "Set"

    def is_record(self) -> bool:
        return self.tag == "Record"

    def is_enum(self) -> bool:
        return self.tag == "Enum"

    def record_name(self) -> str:
        return self.args[0]

    def enum_name(self) -> str:
        return self.args[0]


@dataclass
class Place:
    tag: str  # Var | Index | Field
    name: str = ""
    base: Optional["Place"] = None
    base_ty: Optional[Ty] = None
    index: Optional["Expr"] = None


@dataclass
class Expr:
    ty: Ty
    kind: str
    payload: Any = None


@dataclass
class MatchArm:
    pattern: Any
    body: list[Any]


@dataclass
class Stmt:
    tag: str
    payload: Any = None


@dataclass
class Param:
    name: str
    inout: bool
    ty: Ty


@dataclass
class Func:
    name: str
    export: bool
    params: list[Param]
    ret: Optional[Ty]
    body: list[Stmt]


@dataclass
class Test:
    name: str
    body: list[Stmt]


@dataclass
class Record:
    name: str
    fields: list[tuple[str, Ty]]


@dataclass
class Enum:
    name: str
    variants: list[tuple[str, list[tuple[str, Ty]]]]


@dataclass
class Const:
    name: str
    ty: Ty
    value: Expr


@dataclass
class Module:
    name: str
    imports: list[str]
    records: list[Record]
    enums: list[Enum]
    consts: list[Const]
    funcs: list[Func]
    tests: list[Test]


@dataclass
class EmitReq:
    entry: str
    with_tests: bool
    modules: list[Module]


# ---------------------------------------------------------------------------
# Decode
# ---------------------------------------------------------------------------


def decode_request(v: Any) -> EmitReq:
    obj = expect_keys(["protocol", "cmd", "entry", "with_tests", "modules"], v, "request")
    proto = obj["protocol"]
    if proto != 4:
        raise DecErr(
            f"PROTOCOL MISMATCH: request stamped protocol {proto!r} "
            "but this emitter speaks protocol 4 (mismatched sudoc/backend toolchain pair)"
        )
    cmd = as_str(obj["cmd"], "cmd")
    if cmd != "emit":
        raise DecErr(f"unknown cmd: {cmd}")
    entry = as_str(obj["entry"], "entry")
    with_t = as_bool(obj["with_tests"], "with_tests")
    mods_v = as_arr(obj["modules"], "modules")
    if not mods_v:
        raise DecErr("modules must be non-empty")
    mods = [decode_module(m) for m in mods_v]
    if mods[-1].name != entry:
        raise DecErr(f"entry {entry!r} != last module {mods[-1].name!r}")
    return EmitReq(entry, with_t, mods)


def decode_module(v: Any) -> Module:
    obj = expect_keys(
        ["name", "imports", "records", "enums", "consts", "funcs", "tests"], v, "module"
    )
    return Module(
        name=as_str(obj["name"], "module.name"),
        imports=[as_str(x, "import") for x in as_arr(obj["imports"], "imports")],
        records=[decode_record(x) for x in as_arr(obj["records"], "records")],
        enums=[decode_enum(x) for x in as_arr(obj["enums"], "enums")],
        consts=[decode_const(x) for x in as_arr(obj["consts"], "consts")],
        funcs=[decode_func(x) for x in as_arr(obj["funcs"], "funcs")],
        tests=[decode_test(x) for x in as_arr(obj["tests"], "tests")],
    )


def decode_field_pair(v: Any) -> tuple[str, Ty]:
    obj = expect_keys(["name", "ty", "boundary"], v, "field")
    _ = obj["boundary"]
    return as_str(obj["name"], "field.name"), decode_ty(obj["ty"])


def decode_record(v: Any) -> Record:
    obj = expect_keys(["name", "fields"], v, "record")
    return Record(
        as_str(obj["name"], "record.name"),
        [decode_field_pair(x) for x in as_arr(obj["fields"], "fields")],
    )


def decode_enum(v: Any) -> Enum:
    obj = expect_keys(["name", "variants"], v, "enum")
    variants = []
    for xv in as_arr(obj["variants"], "variants"):
        vo = expect_keys(["name", "fields"], xv, "variant")
        variants.append(
            (
                as_str(vo["name"], "variant.name"),
                [decode_field_pair(x) for x in as_arr(vo["fields"], "variant.fields")],
            )
        )
    return Enum(as_str(obj["name"], "enum.name"), variants)


def decode_const(v: Any) -> Const:
    obj = expect_keys(["name", "ty", "value"], v, "const")
    return Const(
        as_str(obj["name"], "const.name"),
        decode_ty(obj["ty"]),
        decode_expr(obj["value"]),
    )


def decode_func(v: Any) -> Func:
    obj = expect_keys(["name", "export", "params", "ret", "ret_boundary", "body"], v, "func")
    _ = obj["ret_boundary"]
    ret = obj["ret"]
    return Func(
        as_str(obj["name"], "func.name"),
        as_bool(obj["export"], "export"),
        [decode_param(x) for x in as_arr(obj["params"], "params")],
        None if ret is None else decode_ty(ret),
        [decode_stmt(x) for x in as_arr(obj["body"], "body")],
    )


def decode_param(v: Any) -> Param:
    obj = expect_keys(["name", "inout", "ty", "boundary", "never_written"], v, "param")
    _ = obj["boundary"]
    _ = obj["never_written"]
    return Param(
        as_str(obj["name"], "param.name"),
        as_bool(obj["inout"], "inout"),
        decode_ty(obj["ty"]),
    )


def decode_test(v: Any) -> Test:
    obj = expect_keys(["name", "body"], v, "test")
    return Test(as_str(obj["name"], "test.name"), [decode_stmt(x) for x in as_arr(obj["body"], "body")])


def decode_maybe_ty(v: Any) -> Optional[Ty]:
    if v is None:
        return None
    return decode_ty(v)


def decode_ty(v: Any) -> Ty:
    tag, payload = ext_tag(v, "Ty")
    if tag in ("Int", "Float", "Bool") and payload is None:
        return Ty(tag)
    if tag == "List":
        return Ty("List", (decode_ty(payload),))
    if tag == "Set":
        return Ty("Set", (decode_ty(payload),))
    if tag == "Map":
        arr = as_arr(payload, "Map")
        if len(arr) != 2:
            raise DecErr("Map expects [k,v]")
        return Ty("Map", (decode_ty(arr[0]), decode_ty(arr[1])))
    if tag == "Option_":
        return Ty("Option", (decode_ty(payload),))
    if tag == "Result_":
        arr = as_arr(payload, "Result_")
        if len(arr) != 2:
            raise DecErr("Result_ expects [t,e]")
        return Ty("Result", (decode_ty(arr[0]), decode_ty(arr[1])))
    if tag == "Tuple":
        return Ty("Tuple", tuple(decode_ty(x) for x in as_arr(payload, "Tuple")))
    if tag == "Func":
        obj = expect_keys(["params", "ret"], payload, "Func")
        ps = tuple(decode_ty(x) for x in as_arr(obj["params"], "Func.params"))
        return Ty("Func", (ps, decode_maybe_ty(obj["ret"])))
    if tag == "Record":
        return Ty("Record", (as_str(payload, "Record"),))
    if tag == "Enum":
        return Ty("Enum", (as_str(payload, "Enum"),))
    raise DecErr(f"unknown Ty tag: {tag}")


UNARY = {"Neg", "Not"}
BINARY = {
    "Add",
    "Sub",
    "Mul",
    "Div",
    "Mod",
    "Lt",
    "Le",
    "Gt",
    "Ge",
    "Eq",
    "Ne",
    "And",
    "Or",
}
BUILTINS = {
    "AbsInt",
    "AbsFloat",
    "MinInt",
    "MaxInt",
    "MinFloat",
    "MaxFloat",
    "FloatOfInt",
    "IntOfFloat",
    "Floor",
    "Ceil",
    "Round",
    "Sqrt",
    "Filled",
    "NewMap",
    "NewSet",
    "ListLength",
    "ListAppend",
    "ListPop",
    "ListInsert",
    "ListRemoveAt",
    "ListSwap",
    "ListSort",
    "MapSize",
    "MapGet",
    "MapHas",
    "MapDelete",
    "MapKeys",
    "MapValues",
    "SetSize",
    "SetAdd",
    "SetHas",
    "SetRemove",
    "SetItems",
    "OptIsSome",
    "OptIsNone",
    "OptUnwrap",
    "OptGetOr",
    "ResIsOk",
    "ResIsErr",
    "ResUnwrap",
    "ResGetOr",
}


def decode_expr(v: Any) -> Expr:
    obj = expect_keys(["ty", "kind"], v, "expr")
    return Expr(decode_ty(obj["ty"]), *decode_expr_kind(obj["kind"]))


def decode_expr_kind(v: Any) -> tuple[str, Any]:
    tag, p = ext_tag(v, "IrExprKind")
    if tag == "Int":
        return tag, as_i64_str(p, "Int")
    if tag == "Float":
        return tag, as_float(p, "Float")
    if tag == "Bool":
        return tag, as_bool(p, "Bool")
    if tag == "Text":
        return tag, [as_i64_num(x, "Text") for x in as_arr(p, "Text")]
    if tag in ("Local", "Const", "FuncRef"):
        return tag, as_str(p, tag)
    if tag in ("List", "Tuple"):
        return tag, [decode_expr(x) for x in as_arr(p, tag)]
    if tag == "CallFunc":
        obj = expect_keys(["name", "args"], p, "CallFunc")
        return tag, (as_str(obj["name"], "CallFunc.name"), [decode_expr(x) for x in as_arr(obj["args"], "args")])
    if tag == "CallValue":
        obj = expect_keys(["callee", "args"], p, "CallValue")
        return tag, (decode_expr(obj["callee"]), [decode_expr(x) for x in as_arr(obj["args"], "args")])
    if tag == "NewRecord":
        obj = expect_keys(["name", "args"], p, "NewRecord")
        return tag, (as_str(obj["name"], "NewRecord.name"), [decode_expr(x) for x in as_arr(obj["args"], "args")])
    if tag == "NewVariant":
        obj = expect_keys(["enum_name", "variant", "args"], p, "NewVariant")
        return tag, (
            as_str(obj["enum_name"], "enum_name"),
            as_str(obj["variant"], "variant"),
            [decode_expr(x) for x in as_arr(obj["args"], "args")],
        )
    if tag == "Builtin":
        obj = expect_keys(["builtin", "args"], p, "Builtin")
        b = as_str(obj["builtin"], "builtin")
        if b not in BUILTINS:
            raise DecErr(f"unknown Builtin: {b}")
        return tag, (b, [decode_expr(x) for x in as_arr(obj["args"], "args")])
    if tag == "MutBuiltin":
        obj = expect_keys(["builtin", "recv", "recv_ty", "args"], p, "MutBuiltin")
        b = as_str(obj["builtin"], "mut.builtin")
        if b not in BUILTINS:
            raise DecErr(f"unknown Builtin: {b}")
        return tag, (
            b,
            decode_place(obj["recv"]),
            decode_ty(obj["recv_ty"]),
            [decode_expr(x) for x in as_arr(obj["args"], "args")],
        )
    if tag == "GetField":
        obj = expect_keys(["recv", "name"], p, "GetField")
        return tag, (decode_expr(obj["recv"]), as_str(obj["name"], "GetField.name"))
    if tag == "Index":
        obj = expect_keys(["recv", "index"], p, "Index")
        return tag, (decode_expr(obj["recv"]), decode_expr(obj["index"]))
    if tag == "Unary":
        obj = expect_keys(["op", "operand"], p, "Unary")
        op = as_str(obj["op"], "Unary.op")
        if op not in UNARY:
            raise DecErr(f"unknown UnaryOp: {op}")
        return tag, (op, decode_expr(obj["operand"]))
    if tag == "Binary":
        obj = expect_keys(["op", "lhs", "rhs"], p, "Binary")
        op = as_str(obj["op"], "Binary.op")
        if op not in BINARY:
            raise DecErr(f"unknown BinaryOp: {op}")
        return tag, (op, decode_expr(obj["lhs"]), decode_expr(obj["rhs"]))
    raise DecErr(f"unknown IrExprKind: {tag}")


def decode_place(v: Any) -> Place:
    tag, p = ext_tag(v, "Place")
    if tag == "Var":
        return Place("Var", name=as_str(p, "Place.Var"))
    if tag == "Index":
        obj = expect_keys(["base", "base_ty", "index"], p, "Place.Index")
        return Place(
            "Index",
            base=decode_place(obj["base"]),
            base_ty=decode_ty(obj["base_ty"]),
            index=decode_expr(obj["index"]),
        )
    if tag == "Field":
        obj = expect_keys(["base", "base_ty", "name"], p, "Place.Field")
        return Place(
            "Field",
            name=as_str(obj["name"], "Place.Field.name"),
            base=decode_place(obj["base"]),
            base_ty=decode_ty(obj["base_ty"]),
        )
    raise DecErr(f"unknown Place: {tag}")


def decode_stmt(v: Any) -> Stmt:
    tag, p = ext_tag(v, "IrStmt")
    if tag in ("Skip", "Break", "Continue") and p is None:
        return Stmt(tag)
    if tag == "Assign":
        obj = expect_keys(["target", "value", "declares"], p, "Assign")
        return Stmt(tag, (decode_place(obj["target"]), decode_expr(obj["value"]), as_bool(obj["declares"], "declares")))
    if tag == "TupleAssign":
        obj = expect_keys(["targets", "declares", "value"], p, "TupleAssign")
        ts = [as_str(x, "target") for x in as_arr(obj["targets"], "targets")]
        ds = [as_bool(x, "decl") for x in as_arr(obj["declares"], "declares")]
        return Stmt(tag, (ts, ds, decode_expr(obj["value"])))
    if tag == "Expr":
        return Stmt(tag, decode_expr(p))
    if tag == "If":
        obj = expect_keys(["arms", "else_block"], p, "If")
        arms = []
        for a in as_arr(obj["arms"], "If.arms"):
            arr = as_arr(a, "If.arm")
            if len(arr) != 2:
                raise DecErr("If arm must be [cond, body[]]")
            arms.append((decode_expr(arr[0]), [decode_stmt(x) for x in as_arr(arr[1], "If.body")]))
        eb = obj["else_block"]
        else_b = None if eb is None else [decode_stmt(x) for x in as_arr(eb, "else_block")]
        return Stmt(tag, (arms, else_b))
    if tag == "While":
        obj = expect_keys(["cond", "body"], p, "While")
        return Stmt(tag, (decode_expr(obj["cond"]), [decode_stmt(x) for x in as_arr(obj["body"], "While.body")]))
    if tag == "ForRange":
        obj = expect_keys(["var", "from", "to", "down", "body"], p, "ForRange")
        return Stmt(
            tag,
            (
                as_str(obj["var"], "ForRange.var"),
                decode_expr(obj["from"]),
                decode_expr(obj["to"]),
                as_bool(obj["down"], "down"),
                [decode_stmt(x) for x in as_arr(obj["body"], "ForRange.body")],
            ),
        )
    if tag == "ForIn":
        obj = expect_keys(["vars", "iter", "body"], p, "ForIn")
        return Stmt(
            tag,
            (
                [as_str(x, "ForIn.var") for x in as_arr(obj["vars"], "vars")],
                decode_expr(obj["iter"]),
                [decode_stmt(x) for x in as_arr(obj["body"], "ForIn.body")],
            ),
        )
    if tag == "Match":
        obj = expect_keys(["scrutinee", "arms"], p, "Match")
        arms = []
        for a in as_arr(obj["arms"], "Match.arms"):
            ao = expect_keys(["pattern", "body"], a, "MatchArm")
            arms.append(MatchArm(decode_pattern(ao["pattern"]), [decode_stmt(x) for x in as_arr(ao["body"], "arm.body")]))
        return Stmt(tag, (decode_expr(obj["scrutinee"]), arms))
    if tag == "Return":
        if p is None:
            return Stmt(tag, None)
        return Stmt(tag, decode_expr(p))
    if tag == "Assert":
        obj = expect_keys(["cond", "line"], p, "Assert")
        return Stmt(tag, (decode_expr(obj["cond"]), as_line(obj["line"], "Assert.line")))
    if tag == "ExpectTrap":
        obj = expect_keys(["kind", "body", "line"], p, "ExpectTrap")
        return Stmt(
            tag,
            (
                as_str(obj["kind"], "ExpectTrap.kind"),
                [decode_stmt(x) for x in as_arr(obj["body"], "ExpectTrap.body")],
                as_line(obj["line"], "ExpectTrap.line"),
            ),
        )
    raise DecErr(f"unknown IrStmt: {tag}")


def decode_pattern(v: Any) -> Any:
    tag, p = ext_tag(v, "IrPattern")
    if tag == "Int":
        return ("Int", as_i64_str(p, "Pat.Int"))
    if tag == "Bool":
        return ("Bool", as_bool(p, "Pat.Bool"))
    if tag == "Wildcard" and p is None:
        return ("Wildcard",)
    if tag == "Variant":
        obj = expect_keys(["enum_name", "variant", "binders"], p, "Pat.Variant")
        return (
            "Variant",
            as_str(obj["enum_name"], "enum_name"),
            as_str(obj["variant"], "variant"),
            [as_str(x, "binder") for x in as_arr(obj["binders"], "binders")],
        )
    raise DecErr(f"unknown IrPattern: {tag}")


# ---------------------------------------------------------------------------
# Naming
# ---------------------------------------------------------------------------

LEAN_RESERVED = frozenset(
    """
    abbrev axiom class deriving do else end example extends forall fun have if
    import inductive infix infixl infixr instance let macro match namespace
    noncomputable opaque open prelude private protected rec section structure
    syntax then theorem universe variable where with return unless try catch
    finally for in break continue mut discard by calc conv sorry quit exit
    Type Sort Prop Unit Bool true false Option none some Except Array List
    Float Int Nat String Char IO Id pure bind main at from show this Prop
    Inhabited Repr BEq Ord ToString Eq Decidable
    set_option attribute initialize export using raw local scoped notation
    prefix postfix syntax elab command tactic
    def lemma meta mutual partial unsafe constant match_syntax
    repeat skip rename constructor unfolding intro exact apply simp rw
    with_reducible with_unfolding_all focus try first all_goals any_goals
    deriving_instance unif_hint register_simp_attr
    run_elab run_cmd run_tac generalize_proofs hide_aux_declares
    """.split()
)


def enc_len(s: str) -> str:
    return f"{len(s)}{s}"


def _needs_escape(n: str) -> bool:
    """Lean keywords win even as prefixes: `match_at` lexes as `match` + `_at`."""
    if not n:
        return False
    if n in LEAN_RESERVED or n.lower() in LEAN_RESERVED:
        return True
    head = n.split("_", 1)[0]
    return head in LEAN_RESERVED or head.lower() in LEAN_RESERVED


def lean_ident(n: str) -> str:
    return f"«{n}»" if _needs_escape(n) else n


def mangle_value(n: str) -> str:
    base = n
    if base and base[0].isupper():
        base = "v_" + n
    if _needs_escape(base):
        return f"«{base}»"
    return base


def mangle_type(n: str) -> str:
    if not n:
        return "T"
    if n[0].islower():
        base = n[0].upper() + n[1:]
    else:
        base = n
    if base in LEAN_RESERVED or base.lower() in LEAN_RESERVED:
        base = base + "_"
    return base


def mangle_module(n: str) -> str:
    return mangle_type(n)


def mangle_field(rec: str, field: str) -> str:
    # Dots are illegal in Lean binders (`Shape.Rect.w` must not appear raw).
    rec = rec.replace(".", "_")
    field = field.replace(".", "_")
    return "sudo_" + enc_len(mangle_type(rec)) + "_" + enc_len(field)


def mangle_variant(en: str, vn: str) -> str:
    return "Sudo_" + enc_len(mangle_type(en)) + "_" + enc_len(mangle_type(vn))


def split_qual(s: str) -> tuple[Optional[str], str]:
    if "." in s:
        a, b = s.split(".", 1)
        if a and b:
            return a, b
    return None, s


def sanitize_test(name: str) -> str:
    out: list[str] = []
    prev_us = False
    for c in name:
        if c.isascii() and c.isalnum():
            out.append(c.lower())
            prev_us = False
        elif not prev_us and out:
            out.append("_")
            prev_us = True
    while out and out[-1] == "_":
        out.pop()
    return "".join(out) if out else "t"


def test_fn_names(tests: list[Test]) -> list[str]:
    used: set[str] = set()
    out: list[str] = []
    for t in tests:
        san = sanitize_test(t.name)
        cand = f"test_{san}"
        n = 2
        while cand in used:
            cand = f"test_{san}_{n}"
            n += 1
        used.add(cand)
        out.append(cand)
    return out


def type_home(all_mods: list[Module], name: str) -> Optional[str]:
    for m in all_mods:
        if any(r.name == name for r in m.records) or any(e.name == name for e in m.enums):
            return m.name
    return None


# ---------------------------------------------------------------------------
# Places / hoist
# ---------------------------------------------------------------------------


def place_root(p: Place) -> str:
    if p.tag == "Var":
        return p.name
    assert p.base is not None
    return place_root(p.base)


def place_root_of_expr(e: Expr) -> str:
    if e.kind == "Local":
        return e.payload
    if e.kind == "GetField":
        return place_root_of_expr(e.payload[0])
    if e.kind == "Index":
        return place_root_of_expr(e.payload[0])
    return "?"


class Fresh:
    def __init__(self) -> None:
        self.n = 0

    def name(self, prefix: str = "_t") -> str:
        self.n += 1
        return f"{prefix}{self.n}"


def hoist_expr(e: Expr, fresh: Fresh) -> tuple[list[Stmt], Expr]:
    """Lift MutBuiltin (and nested) into preceding Assign stmts."""
    extra: list[Stmt] = []

    def go(x: Expr) -> Expr:
        nonlocal extra
        k, p = x.kind, x.payload
        if k == "MutBuiltin":
            b, recv, rty, args = p
            recv2, rextra = hoist_place(recv, fresh)
            extra.extend(rextra)
            args2 = [go(a) for a in args]
            tmp = fresh.name("_hm")
            extra.append(
                Stmt("Assign", (Place("Var", name=tmp), Expr(x.ty, "MutBuiltin", (b, recv2, rty, args2)), True))
            )
            return Expr(x.ty, "Local", tmp)
        if k in ("List", "Tuple"):
            return Expr(x.ty, k, [go(a) for a in p])
        if k == "CallFunc":
            return Expr(x.ty, k, (p[0], [go(a) for a in p[1]]))
        if k == "CallValue":
            return Expr(x.ty, k, (go(p[0]), [go(a) for a in p[1]]))
        if k == "NewRecord":
            return Expr(x.ty, k, (p[0], [go(a) for a in p[1]]))
        if k == "NewVariant":
            return Expr(x.ty, k, (p[0], p[1], [go(a) for a in p[2]]))
        if k == "Builtin":
            return Expr(x.ty, k, (p[0], [go(a) for a in p[1]]))
        if k == "GetField":
            return Expr(x.ty, k, (go(p[0]), p[1]))
        if k == "Index":
            return Expr(x.ty, k, (go(p[0]), go(p[1])))
        if k == "Unary":
            return Expr(x.ty, k, (p[0], go(p[1])))
        if k == "Binary":
            return Expr(x.ty, k, (p[0], go(p[1]), go(p[2])))
        return x

    return extra, go(e)


def hoist_place(p: Place, fresh: Fresh) -> tuple[Place, list[Stmt]]:
    if p.tag == "Var":
        return p, []
    if p.tag == "Index":
        assert p.base is not None and p.index is not None
        b, be = hoist_place(p.base, fresh)
        ie, x = hoist_expr(p.index, fresh)
        return Place("Index", base=b, base_ty=p.base_ty, index=x), be + ie
    if p.tag == "Field":
        assert p.base is not None
        b, be = hoist_place(p.base, fresh)
        return Place("Field", name=p.name, base=b, base_ty=p.base_ty), be
    return p, []


def hoist_stmts(stmts: list[Stmt], fresh: Fresh) -> list[Stmt]:
    out: list[Stmt] = []
    for s in stmts:
        out.extend(hoist_stmt(s, fresh))
    return out


def hoist_stmt(s: Stmt, fresh: Fresh) -> list[Stmt]:
    t, p = s.tag, s.payload
    if t == "Assign":
        target, value, declares = p
        target2, te = hoist_place(target, fresh)
        extra, value2 = hoist_expr(value, fresh)
        return te + extra + [Stmt("Assign", (target2, value2, declares))]
    if t == "TupleAssign":
        ts, ds, value = p
        extra, value2 = hoist_expr(value, fresh)
        return extra + [Stmt("TupleAssign", (ts, ds, value2))]
    if t == "Expr":
        extra, e2 = hoist_expr(p, fresh)
        return extra + [Stmt("Expr", e2)]
    if t == "If":
        arms, else_b = p
        new_arms = []
        prefix: list[Stmt] = []
        for c, b in arms:
            extra, c2 = hoist_expr(c, fresh)
            prefix.extend(extra)
            new_arms.append((c2, hoist_stmts(b, fresh)))
        else2 = None if else_b is None else hoist_stmts(else_b, fresh)
        return prefix + [Stmt("If", (new_arms, else2))]
    if t == "While":
        c, b = p
        extra, c2 = hoist_expr(c, fresh)
        return extra + [Stmt("While", (c2, hoist_stmts(b, fresh)))]
    if t == "ForRange":
        var, fr, to, down, b = p
        e1, fr2 = hoist_expr(fr, fresh)
        e2, to2 = hoist_expr(to, fresh)
        return e1 + e2 + [Stmt("ForRange", (var, fr2, to2, down, hoist_stmts(b, fresh)))]
    if t == "ForIn":
        vs, it, b = p
        extra, it2 = hoist_expr(it, fresh)
        return extra + [Stmt("ForIn", (vs, it2, hoist_stmts(b, fresh)))]
    if t == "Match":
        sc, arms = p
        extra, sc2 = hoist_expr(sc, fresh)
        arms2 = [MatchArm(a.pattern, hoist_stmts(a.body, fresh)) for a in arms]
        return extra + [Stmt("Match", (sc2, arms2))]
    if t == "Return" and p is not None:
        extra, e2 = hoist_expr(p, fresh)
        return extra + [Stmt("Return", e2)]
    if t == "Assert":
        c, line = p
        extra, c2 = hoist_expr(c, fresh)
        return extra + [Stmt("Assert", (c2, line))]
    if t == "ExpectTrap":
        k, b, line = p
        return [Stmt("ExpectTrap", (k, hoist_stmts(b, fresh), line))]
    return [s]


# ---------------------------------------------------------------------------
# Loop threaded vars
# ---------------------------------------------------------------------------


def collect_declared(stmts: list[Stmt]) -> list[str]:
    out: list[str] = []
    for s in stmts:
        t, p = s.tag, s.payload
        if t == "Assign":
            target, _v, declares = p
            if declares and target.tag == "Var":
                out.append(target.name)
        elif t == "TupleAssign":
            ts, ds, _v = p
            out.extend(x for x, d in zip(ts, ds) if d)
        elif t == "If":
            arms, else_b = p
            for _c, b in arms:
                out.extend(collect_declared(b))
            if else_b:
                out.extend(collect_declared(else_b))
        elif t == "While":
            out.extend(collect_declared(p[1]))
        elif t == "ForRange":
            out.append(p[0])
            out.extend(collect_declared(p[4]))
        elif t == "ForIn":
            out.extend(p[0])
            out.extend(collect_declared(p[2]))
        elif t == "Match":
            for a in p[1]:
                if a.pattern[0] == "Variant":
                    out.extend(a.pattern[3])
                out.extend(collect_declared(a.body))
        elif t == "ExpectTrap":
            out.extend(collect_declared(p[1]))
    return out


def lookup_func(mods: list[Module], cur: Module, name: str) -> Optional[Func]:
    mq, fn = split_qual(name)
    if mq is None:
        for f in cur.funcs:
            if f.name == fn:
                return f
        return None
    for m in mods:
        if m.name == mq:
            for f in m.funcs:
                if f.name == fn:
                    return f
    return None


def _walk_expr_calls(e: Expr, out: list[str]) -> None:
    k, p = e.kind, e.payload
    if k == "CallFunc":
        out.append(p[0])
        for a in p[1]:
            _walk_expr_calls(a, out)
    elif k == "CallValue":
        _walk_expr_calls(p[0], out)
        for a in p[1]:
            _walk_expr_calls(a, out)
    elif k in ("List", "Tuple"):
        for a in p:
            _walk_expr_calls(a, out)
    elif k == "NewRecord":
        for a in p[1]:
            _walk_expr_calls(a, out)
    elif k == "NewVariant":
        for a in p[2]:
            _walk_expr_calls(a, out)
    elif k == "Builtin":
        for a in p[1]:
            _walk_expr_calls(a, out)
    elif k == "MutBuiltin":
        _walk_place_calls(p[1], out)
        for a in p[3]:
            _walk_expr_calls(a, out)
    elif k == "GetField":
        _walk_expr_calls(p[0], out)
    elif k == "Index":
        _walk_expr_calls(p[0], out)
        _walk_expr_calls(p[1], out)
    elif k == "Unary":
        _walk_expr_calls(p[1], out)
    elif k == "Binary":
        _walk_expr_calls(p[1], out)
        _walk_expr_calls(p[2], out)
    elif k == "FuncRef":
        out.append(p)


def _walk_place_calls(p: Place, out: list[str]) -> None:
    if p.tag == "Index" and p.index is not None:
        if p.base is not None:
            _walk_place_calls(p.base, out)
        _walk_expr_calls(p.index, out)
    elif p.tag == "Field" and p.base is not None:
        _walk_place_calls(p.base, out)


def _walk_stmts_calls(stmts: list[Stmt], out: list[str]) -> None:
    for s in stmts:
        t, p = s.tag, s.payload
        if t == "Assign":
            _walk_place_calls(p[0], out)
            _walk_expr_calls(p[1], out)
        elif t == "TupleAssign":
            _walk_expr_calls(p[2], out)
        elif t == "Expr":
            _walk_expr_calls(p, out)
        elif t == "Assert":
            _walk_expr_calls(p[0], out)
        elif t == "If":
            for c, b in p[0]:
                _walk_expr_calls(c, out)
                _walk_stmts_calls(b, out)
            if p[1] is not None:
                _walk_stmts_calls(p[1], out)
        elif t == "Match":
            _walk_expr_calls(p[0], out)
            for arm in p[1]:
                _walk_stmts_calls(arm.body, out)
        elif t == "While":
            _walk_expr_calls(p[0], out)
            _walk_stmts_calls(p[1], out)
        elif t == "ForRange":
            _walk_expr_calls(p[1], out)
            _walk_expr_calls(p[2], out)
            _walk_stmts_calls(p[4], out)
        elif t == "ForIn":
            _walk_expr_calls(p[1], out)
            _walk_stmts_calls(p[2], out)
        elif t == "ExpectTrap":
            _walk_stmts_calls(p[1], out)
        elif t == "Return" and p is not None:
            _walk_expr_calls(p, out)


def module_func_sccs(m: Module) -> tuple[list[list[str]], dict[str, list[str]]]:
    names = [f.name for f in m.funcs]
    local = set(names)
    deps: dict[str, list[str]] = {n: [] for n in names}
    for f in m.funcs:
        raw: list[str] = []
        _walk_stmts_calls(f.body, raw)
        seen: set[str] = set()
        for cal in raw:
            _mq, fn = split_qual(cal)
            if fn in local and fn not in seen:
                deps[f.name].append(fn)
                seen.add(fn)
    return _tarjan_sccs(names, deps), deps


def ty_nominals(t: Ty) -> list[str]:
    if t.tag in ("Record", "Enum"):
        return [t.args[0]]
    out: list[str] = []
    if t.tag in ("List", "Set", "Option"):
        out.extend(ty_nominals(t.args[0]))
    elif t.tag in ("Map", "Result"):
        out.extend(ty_nominals(t.args[0]))
        out.extend(ty_nominals(t.args[1]))
    elif t.tag == "Tuple":
        for a in t.args:
            out.extend(ty_nominals(a))
    elif t.tag == "Func":
        ps, ret = t.args
        for p in ps:
            out.extend(ty_nominals(p))
        if ret is not None:
            out.extend(ty_nominals(ret))
    return out


def module_type_sccs(m: Module) -> list[list[str]]:
    """SCCs of module-local records/enums, dependencies first."""
    names = [r.name for r in m.records] + [e.name for e in m.enums]
    local = set(names)
    deps: dict[str, list[str]] = {n: [] for n in names}
    for r in m.records:
        seen: set[str] = set()
        for _, ty in r.fields:
            for n in ty_nominals(ty):
                if n in local and n not in seen:
                    deps[r.name].append(n)
                    seen.add(n)
    for e in m.enums:
        seen = set()
        for _, fields in e.variants:
            for _, ty in fields:
                for n in ty_nominals(ty):
                    if n in local and n not in seen:
                        deps[e.name].append(n)
                        seen.add(n)
    return _tarjan_sccs(names, deps)


def _tarjan_sccs(nodes: list[str], deps: dict[str, list[str]]) -> list[list[str]]:
    index = 0
    stack: list[str] = []
    on: set[str] = set()
    idx: dict[str, int] = {}
    low: dict[str, int] = {}
    sccs: list[list[str]] = []

    def strongconnect(v: str) -> None:
        nonlocal index
        idx[v] = index
        low[v] = index
        index += 1
        stack.append(v)
        on.add(v)
        for w in deps.get(v, []):
            if w not in idx:
                strongconnect(w)
                low[v] = min(low[v], low[w])
            elif w in on:
                low[v] = min(low[v], idx[w])
        if low[v] == idx[v]:
            comp: list[str] = []
            while True:
                w = stack.pop()
                on.remove(w)
                comp.append(w)
                if w == v:
                    break
            sccs.append(comp)

    for v in nodes:
        if v not in idx:
            strongconnect(v)
    return sccs


def inductive_record_names(m: Module) -> set[str]:
    """Records that sit in a multi-node SCC must be inductives: Lean 4.14
    rejects mixing `structure` and `inductive` in one `mutual` block."""
    recs = {r.name for r in m.records}
    out: set[str] = set()
    for scc in module_type_sccs(m):
        if len(scc) > 1:
            out.update(n for n in scc if n in recs)
    return out


def collect_threaded(mods: list[Module], cur: Module, body: list[Stmt]) -> list[str]:
    declared = set(collect_declared(body))
    found: list[str] = []

    def add(n: str) -> None:
        if n and n != "?" and n not in declared and n not in found:
            found.append(n)

    def from_expr(e: Expr) -> None:
        if e.kind == "MutBuiltin":
            add(place_root(e.payload[1]))
        elif e.kind == "CallFunc":
            f = lookup_func(mods, cur, e.payload[0])
            if f:
                for param, arg in zip(f.params, e.payload[1]):
                    if param.inout:
                        add(place_root_of_expr(arg))

    def walk(stmts: list[Stmt]) -> None:
        for s in stmts:
            t, p = s.tag, s.payload
            if t == "Assign":
                target, value, declares = p
                if not declares:
                    add(place_root(target))
                from_expr(value)
            elif t == "TupleAssign":
                ts, ds, value = p
                for x, d in zip(ts, ds):
                    if not d:
                        add(x)
                from_expr(value)
            elif t == "Expr":
                from_expr(p)
            elif t == "If":
                arms, else_b = p
                for _c, b in arms:
                    walk(b)
                if else_b:
                    walk(else_b)
            elif t == "While":
                walk(p[1])
            elif t == "ForRange":
                walk(p[4])
            elif t == "ForIn":
                walk(p[2])
            elif t == "Match":
                for a in p[1]:
                    walk(a.body)
            elif t == "ExpectTrap":
                walk(p[1])

    walk(body)
    return found


# ---------------------------------------------------------------------------
# Emitter
# ---------------------------------------------------------------------------


class Em:
    def __init__(self, all_mods: list[Module], cur: Module):
        self.all = all_mods
        self.cur = cur
        self.fresh = Fresh()
        self.mode = "expr"  # expr | loop
        self.inouts: list[str] = []
        self.loop_vars: list[str] = []
        self.go_n = 0
        self.inductive_records: set[str] = set()
        for m in all_mods:
            self.inductive_records |= inductive_record_names(m)
        self.fueled: set[str] = set()
        self.current_scc: set[str] = set()
        self.emitting_fueled = False
        sccs, fdeps = module_func_sccs(cur)
        for scc in sccs:
            rec = len(scc) > 1 or (scc and scc[0] in fdeps.get(scc[0], []))
            if rec:
                self.fueled.update(scc)

    def next_go(self) -> str:
        n = self.go_n
        self.go_n += 1
        return self.fresh.name("go")

    def qual_nominal(self, name: str, local: str) -> str:
        home = type_home(self.all, name)
        if home is None:
            raise DecErr(f"internal error: nominal type {name!r} has no declaring module")
        if home != self.cur.name:
            return mangle_module(home) + "." + local
        return local

    def qual_type(self, n: str) -> str:
        return self.qual_nominal(n, mangle_type(n))

    def qual_variant(self, en: str, vn: str) -> str:
        # Constructors live in the inductive's namespace: `Shape.Sudo_…`,
        # and across modules `Home.Shape.Sudo_…`.
        local = mangle_type(en) + "." + mangle_variant(en, vn)
        return self.qual_nominal(en, local)

    def qual_field(self, rn: str, fn: str) -> str:
        return self.qual_nominal(rn, mangle_field(rn, fn))

    def render_ty(self, t: Optional[Ty]) -> str:
        if t is None:
            return "Unit"
        tag = t.tag
        if tag == "Int":
            return "Int"
        if tag == "Float":
            return "Float"
        if tag == "Bool":
            return "Bool"
        if tag == "List":
            return f"Array ({self.render_ty(t.args[0])})"
        if tag == "Set":
            return f"SudoRt.SSet ({self.render_ty(t.args[0])})"
        if tag == "Map":
            return f"SudoRt.SMap ({self.render_ty(t.args[0])}) ({self.render_ty(t.args[1])})"
        if tag == "Option":
            return f"Option ({self.render_ty(t.args[0])})"
        if tag == "Result":
            # SResult ε α — error first
            return f"SudoRt.SResult ({self.render_ty(t.args[1])}) ({self.render_ty(t.args[0])})"
        if tag == "Tuple":
            if not t.args:
                return "Unit"
            parts = [self.render_ty(x) for x in t.args]
            if len(parts) == 1:
                return parts[0]
            return " × ".join(f"({p})" for p in parts)
        if tag == "Func":
            ps, ret = t.args
            ret_s = f"Except SudoRt.Trap ({self.render_ty(ret)})"
            if not ps:
                return f"Unit → {ret_s}"
            return " → ".join([f"({self.render_ty(p)})" for p in ps] + [ret_s])
        if tag == "Record":
            return self.qual_type(t.args[0])
        if tag == "Enum":
            return self.qual_type(t.args[0])
        raise DecErr(f"unhandled Ty: {tag}")

    def fret_tys(self, f: Optional[Func]) -> list[Ty]:
        if f is None:
            return []
        out: list[Ty] = []
        if f.ret is not None:
            out.append(f.ret)
        out.extend(p.ty for p in f.params if p.inout)
        return out

    def fret_ty_str(self, f: Optional[Func]) -> str:
        parts = self.fret_tys(f)
        if not parts:
            return "Unit"
        if len(parts) == 1:
            return self.render_ty(parts[0])
        return " × ".join(f"({self.render_ty(t)})" for t in parts)

    def flow_ty(self, f: Optional[Func]) -> str:
        sigma = self.sigma_ty()
        rho = self.fret_ty_str(f) if f is not None else self.rho_fallback()
        return f"SudoRt.Flow ({sigma}) ({rho})"

    def rho_str(self, f: Optional[Func]) -> str:
        return self.fret_ty_str(f)

    def flow_ctor(self, ctor: str, val: str, f: Optional[Func]) -> str:
        return f"(SudoRt.Flow.{ctor} (ρ := {self.rho_str(f)}) {val})"

    def except_flow_ty(self, f: Optional[Func]) -> str:
        return f"Except SudoRt.Trap (SudoRt.Flow _ ({self.rho_str(f)}))"

    def _do_as(self, lines: list[str], ty: str) -> str:
        return f"({self._do(lines)} : {ty})"

    def rho_fallback(self) -> str:
        if not self.inouts:
            return "Unit"
        if len(self.inouts) == 1:
            return "Int"  # overwritten when we know types; see emit_func
        return "Unit"

    def sigma_ty(self) -> str:
        if not self.loop_vars:
            return "Unit"
        # Types of loop vars are not stored; use a type hole via the values we
        # already have in scope — Lean infers Flow's σ from the payload.
        # We still need a type ascription on `let rec`. Emit `Unit` for 0,
        # otherwise let Lean infer by omitting the ascription on Flow.
        return "_σ"

    def sigma_term(self) -> str:
        vs = [mangle_value(v) for v in self.loop_vars]
        if not vs:
            return "()"
        if len(vs) == 1:
            return vs[0]
        return "(" + ", ".join(vs) + ")"

    def sigma_pat(self) -> str:
        return self.sigma_term()

    def call_name(self, name: str) -> str:
        mq, fn = split_qual(name)
        if mq:
            return mangle_module(mq) + "." + mangle_value(fn)
        return mangle_value(fn)

    def emit_callee(self, name: str) -> str:
        """Wrapper name, or `foo_go fuel` when compiling a recursive SCC."""
        _mq, fn = split_qual(name)
        if (
            fn in self.fueled
            and self.emitting_fueled
            and fn in self.current_scc
            and (_mq is None or _mq == self.cur.name)
        ):
            return f"{mangle_value(fn)}_go _rfuel"
        return self.call_name(name)

    def const_name(self, name: str) -> str:
        mq, cn = split_qual(name)
        if mq:
            return mangle_module(mq) + "." + mangle_value(cn)
        return mangle_value(cn)

    # -- expressions --------------------------------------------------------

    def emit_int(self, n: int) -> str:
        return f"({n} : Int)"

    def emit_float(self, f: float) -> str:
        if f != f:  # NaN
            return "((0.0 : Float) / 0.0)"
        if f == float("inf"):
            return "((1.0 : Float) / 0.0)"
        if f == float("-inf"):
            return "(((-1.0) : Float) / 0.0)"
        # Preserve −0.0
        if f == 0.0 and str(f).startswith("-"):
            return "(-0.0 : Float)"
        s = repr(f)
        if s.endswith(".0") or "." in s or "e" in s or "E" in s:
            return f"({s} : Float)"
        return f"({s}.0 : Float)"

    def bind_seq(self, exprs: list[Expr]) -> tuple[list[str], list[str]]:
        """Evaluate exprs LTR; return (lines, bound terms)."""
        lines: list[str] = []
        names: list[str] = []
        for e in exprs:
            ls, term = self.emit_expr(e)
            lines.extend(ls)
            names.append(term)
        return lines, names

    def emit_expr(self, e: Expr) -> tuple[list[str], str]:
        k, p = e.kind, e.payload
        if k == "Int":
            return [], self.emit_int(p)
        if k == "Float":
            return [], self.emit_float(p)
        if k == "Bool":
            return [], "true" if p else "false"
        if k == "Text":
            inner = ", ".join(str(n) for n in p)
            return [], f"(#[{inner}] : Array Int)"
        if k == "Local":
            return [], mangle_value(p)
        if k == "Const":
            return [], self.const_name(p)
        if k == "FuncRef":
            return [], self.call_name(p)
        if k == "List":
            ls, ns = self.bind_seq(p)
            body = ", ".join(ns)
            ascr = f" : Array ({self.render_ty(e.ty.args[0])})" if e.ty.is_list() else ""
            return ls, f"(#[{body}]{ascr})"
        if k == "Tuple":
            if not p:
                return [], "()"
            ls, ns = self.bind_seq(p)
            if len(ns) == 1:
                return ls, ns[0]
            return ls, "(" + ", ".join(ns) + ")"
        if k == "CallFunc":
            name, args = p
            ls, ns = self.bind_seq(args)
            call = self.emit_callee(name)
            if not ns:
                tmp = self.fresh.name()
                return ls + [f"let {tmp} ← {call}"], tmp
            tmp = self.fresh.name()
            return ls + [f"let {tmp} ← {call} {' '.join(ns)}"], tmp
        if k == "CallValue":
            cal, args = p
            ls1, c = self.emit_expr(cal)
            ls2, ns = self.bind_seq(args)
            tmp = self.fresh.name()
            return ls1 + ls2 + [f"let {tmp} ← {c} {' '.join(ns)}".rstrip()], tmp
        if k == "NewRecord":
            name, args = p
            ls, ns = self.bind_seq(args)
            rec = next((r for r in self._all_records() if r.name == name), None)
            ctor = self.qual_type(name)
            if name in self.inductive_records:
                return ls, f"({ctor}.mk {' '.join(ns)})".replace(".mk )", ".mk)")
            if rec and rec.fields:
                fields = [self.qual_field(name, fn) for fn, _ty in rec.fields]
                assigns = ", ".join(f"{f} := {v}" for f, v in zip(fields, ns))
                return ls, f"({{ {assigns} }} : {ctor})"
            return ls, f"({ctor}.mk {' '.join(ns)})".replace(".mk )", ".mk)")
        if k == "NewVariant":
            en, vn, args = p
            if en == "Option" and vn == "Some":
                ls, ns = self.bind_seq(args)
                return ls, f"(some {ns[0]})"
            if en == "Option" and vn == "None":
                return [], f"(none : {self.render_ty(e.ty)})"
            if en == "Result" and vn == "Ok":
                ls, ns = self.bind_seq(args)
                return ls, f"(SudoRt.SResult.ok {ns[0]})"
            if en == "Result" and vn == "Err":
                ls, ns = self.bind_seq(args)
                return ls, f"(SudoRt.SResult.err {ns[0]})"
            ls, ns = self.bind_seq(args)
            ctor = self.qual_variant(en, vn)
            if not ns:
                return [], ctor
            return ls, f"({ctor}.mk {' '.join(ns)})" if False else f"({ctor} {' '.join(ns)})"
        if k == "Builtin":
            return self.emit_builtin(p[0], p[1], e.ty)
        if k == "MutBuiltin":
            raise DecErr("internal: MutBuiltin reached emit_expr; hoist failed")
        if k == "GetField":
            recv, name = p
            ls, r = self.emit_expr(recv)
            if recv.ty.is_record():
                fld = self.qual_field(recv.ty.record_name(), name)
                # Field accessor is the local projection; `open`/`namespace` resolve it.
                return ls, f"({r}).{fld.split('.')[-1]}"
            return ls, f"({r}).{name}"
        if k == "Index":
            recv, idx = p
            ls1, r = self.emit_expr(recv)
            ls2, i = self.emit_expr(idx)
            tmp = self.fresh.name()
            if recv.ty.is_map():
                fn = "SudoRt.mapGet"
            else:
                fn = "SudoRt.atL"
            return ls1 + ls2 + [f"let {tmp} ← {fn} {r} {i}"], tmp
        if k == "Unary":
            op, o = p
            ls, a = self.emit_expr(o)
            if op == "Neg":
                if o.ty.is_int():
                    tmp = self.fresh.name()
                    return ls + [f"let {tmp} ← SudoRt.negI {a}"], tmp
                return ls, f"(Float.neg {a})"
            if op == "Not":
                return ls, f"(!( {a} ))"
            raise DecErr(f"unknown unary {op}")
        if k == "Binary":
            return self.emit_binary(p[0], p[1], p[2])
        raise DecErr(f"unknown expr kind {k}")

    def _all_records(self) -> list[Record]:
        out: list[Record] = []
        for m in self.all:
            out.extend(m.records)
        return out

    def emit_binary(self, op: str, l: Expr, r: Expr) -> tuple[list[str], str]:
        if op in ("And", "Or"):
            ls1, a = self.emit_expr(l)
            tmp = self.fresh.name()
            ls2, b = self.emit_expr(r)
            rhs = self._do(ls2 + [f"pure {b}"])
            if op == "And":
                line = f"let {tmp} ← (if {a} then {rhs} else pure false)"
            else:
                line = f"let {tmp} ← (if {a} then pure true else {rhs})"
            return ls1 + [line], tmp
        ls, ns = self.bind_seq([l, r])
        a, b = ns
        tmp = self.fresh.name()
        if op == "Add" and l.ty.is_int():
            return ls + [f"let {tmp} ← SudoRt.addI {a} {b}"], tmp
        if op == "Add" and l.ty.is_list():
            return ls, f"(SudoRt.concatL {a} {b})"
        if op == "Add":
            return ls, f"({a} + {b})"
        if op == "Sub" and l.ty.is_int():
            return ls + [f"let {tmp} ← SudoRt.subI {a} {b}"], tmp
        if op == "Sub":
            return ls, f"({a} - {b})"
        if op == "Mul" and l.ty.is_int():
            return ls + [f"let {tmp} ← SudoRt.mulI {a} {b}"], tmp
        if op == "Mul":
            return ls, f"({a} * {b})"
        if op == "Div" and l.ty.is_int():
            return ls + [f"let {tmp} ← SudoRt.divI {a} {b}"], tmp
        if op == "Div":
            return ls, f"(SudoRt.fdiv {a} {b})"
        if op == "Mod":
            return ls + [f"let {tmp} ← SudoRt.modI {a} {b}"], tmp
        infix = {"Lt": "<", "Le": "≤", "Gt": ">", "Ge": "≥"}
        if op in infix:
            if l.ty.is_int():
                return ls, f"(decide ({a} {infix[op]} {b}))"
            if l.ty.is_float():
                return ls, f"({a} {infix[op]} {b})"
            if op == "Lt":
                return ls, f"(SudoRt.SOrd.le {a} {b} && !(SudoRt.SEq.beq {a} {b}))"
            if op == "Le":
                return ls, f"(SudoRt.SOrd.le {a} {b})"
            if op == "Gt":
                return ls, f"(SudoRt.SOrd.le {b} {a} && !(SudoRt.SEq.beq {a} {b}))"
            return ls, f"(SudoRt.SOrd.le {b} {a})"
        if op == "Eq":
            return ls, f"(SudoRt.SEq.beq {a} {b})"
        if op == "Ne":
            return ls, f"(!(SudoRt.SEq.beq {a} {b}))"
        raise DecErr(f"unknown binary {op}")

    def emit_builtin(self, b: str, args: list[Expr], ty: Ty) -> tuple[list[str], str]:
        ls, ns = self.bind_seq(args)
        a0 = ns[0] if ns else ""
        tmp = self.fresh.name()
        table = {
            "AbsInt": (f"SudoRt.absI {a0}", True),
            "AbsFloat": (f"SudoRt.absF {a0}", False),
            "MinInt": (f"SudoRt.minI {ns[0]} {ns[1]}" if len(ns) > 1 else "", False),
            "MaxInt": (f"SudoRt.maxI {ns[0]} {ns[1]}" if len(ns) > 1 else "", False),
            "MinFloat": (f"SudoRt.fmin {ns[0]} {ns[1]}" if len(ns) > 1 else "", False),
            "MaxFloat": (f"SudoRt.fmax {ns[0]} {ns[1]}" if len(ns) > 1 else "", False),
            "FloatOfInt": (f"SudoRt.floatOfInt {a0}", False),
            "IntOfFloat": (f"SudoRt.intOfFloat {a0}", True),
            "Floor": (f"SudoRt.floorF {a0}", False),
            "Ceil": (f"SudoRt.ceilF {a0}", False),
            "Round": (f"SudoRt.roundHalfAway {a0}", False),
            "Sqrt": (f"SudoRt.sqrtF {a0}", False),
            "Filled": (f"SudoRt.filledL {ns[0]} {ns[1]}" if len(ns) > 1 else "", True),
            "NewMap": (f"(SudoRt.mapNew : {self.render_ty(ty)})", False),
            "NewSet": (f"(SudoRt.setNew : {self.render_ty(ty)})", False),
            "ListLength": (f"SudoRt.listLen {a0}", False),
            "MapSize": (f"SudoRt.mapSize {a0}", False),
            "MapGet": (f"SudoRt.mapGetOpt {ns[0]} {ns[1]}" if len(ns) > 1 else "", False),
            "MapHas": (f"SudoRt.mapHas {ns[0]} {ns[1]}" if len(ns) > 1 else "", False),
            "MapKeys": (f"SudoRt.mapKeysL {a0}", False),
            "MapValues": (f"SudoRt.mapValuesL {a0}", False),
            "SetSize": (f"SudoRt.setSize {a0}", False),
            "SetHas": (f"SudoRt.setHas {ns[0]} {ns[1]}" if len(ns) > 1 else "", False),
            "SetItems": (f"SudoRt.setItemsL {a0}", False),
            "OptIsSome": (f"SudoRt.optIsSome {a0}", False),
            "OptIsNone": (f"SudoRt.optIsNone {a0}", False),
            "OptUnwrap": (f"SudoRt.optUnwrap {a0}", True),
            "OptGetOr": (f"SudoRt.optGetOr {ns[0]} {ns[1]}" if len(ns) > 1 else "", False),
            "ResIsOk": (f"SudoRt.resIsOk {a0}", False),
            "ResIsErr": (f"SudoRt.resIsErr {a0}", False),
            "ResUnwrap": (f"SudoRt.resUnwrap {a0}", True),
            "ResGetOr": (f"SudoRt.resGetOr {ns[0]} {ns[1]}" if len(ns) > 1 else "", False),
        }
        if b not in table:
            raise DecErr(f"builtin {b} is MutBuiltin or unknown")
        call, traps = table[b]
        if traps:
            return ls + [f"let {tmp} ← {call}"], tmp
        return ls, f"({call})" if not call.startswith("(") else call

    # -- places -------------------------------------------------------------

    def force_place_indices(self, p: Place) -> tuple[list[str], Place]:
        """Evaluate index/key exprs on a place (base then index, source order)
        so a later RHS cannot trap first."""
        if p.tag == "Var":
            return [], p
        if p.tag == "Field":
            assert p.base is not None
            ls, base = self.force_place_indices(p.base)
            return ls, Place("Field", name=p.name, base=base, base_ty=p.base_ty)
        if p.tag == "Index":
            assert p.base is not None and p.index is not None
            ls1, base = self.force_place_indices(p.base)
            ls2, i = self.emit_expr(p.index)
            tmp = self.fresh.name("_ix")
            new_idx = Expr(p.index.ty, "Local", tmp)
            return ls1 + ls2 + [f"let {tmp} := {i}"], Place(
                "Index", base=base, base_ty=p.base_ty, index=new_idx
            )
        raise DecErr(f"unknown place {p.tag}")

    def emit_place_get(self, p: Place) -> tuple[list[str], str]:
        if p.tag == "Var":
            return [], mangle_value(p.name)
        if p.tag == "Index":
            assert p.base is not None and p.index is not None and p.base_ty is not None
            ls1, b = self.emit_place_get(p.base)
            ls2, i = self.emit_expr(p.index)
            tmp = self.fresh.name()
            fn = "SudoRt.mapGet" if p.base_ty.is_map() else "SudoRt.atL"
            return ls1 + ls2 + [f"let {tmp} ← {fn} {b} {i}"], tmp
        if p.tag == "Field":
            assert p.base is not None and p.base_ty is not None
            ls, b = self.emit_place_get(p.base)
            if p.base_ty.is_record():
                fld = mangle_field(p.base_ty.record_name(), p.name)
                return ls, f"({b}).{fld}"
            return ls, f"({b}).{p.name}"
        raise DecErr(f"unknown place {p.tag}")

    def emit_place_set(self, p: Place, val: str) -> tuple[list[str], str]:
        """Rebuild root value after writing `val` at place p. Returns (lines, new_root_term)."""
        if p.tag == "Var":
            return [], val
        if p.tag == "Index":
            assert p.base is not None and p.index is not None and p.base_ty is not None
            ls1, b = self.emit_place_get(p.base)
            ls2, i = self.emit_expr(p.index)
            tmp = self.fresh.name()
            if p.base_ty.is_map():
                new_base = f"SudoRt.mapPut {b} {i} {val}"
                lines = ls1 + ls2 + [f"let {tmp} := {new_base}"]
            else:
                new_base = f"SudoRt.putL {b} {i} {val}"
                lines = ls1 + ls2 + [f"let {tmp} ← {new_base}"]
            ls3, root = self.emit_place_set(p.base, tmp)
            return lines + ls3, root
        if p.tag == "Field":
            assert p.base is not None and p.base_ty is not None
            ls, b = self.emit_place_get(p.base)
            if p.base_ty.is_record():
                new_base = self.record_with(p.base_ty.record_name(), b, p.name, val)
            else:
                new_base = f"{{ {b} with {p.name} := {val} }}"
            tmp = self.fresh.name()
            ls3, root = self.emit_place_set(p.base, tmp)
            return ls + [f"let {tmp} := {new_base}"] + ls3, root
        raise DecErr(f"unknown place {p.tag}")

    # -- statements ---------------------------------------------------------

    def _do(self, lines: list[str]) -> str:
        if not lines:
            return "(pure ())"
        body = "\n".join("  " + ln for ln in lines)
        return "(do\n" + body + ")"

    def end_block(self, f: Optional[Func]) -> list[str]:
        if self.mode == "loop":
            return [f"pure {self.flow_ctor('cont', self.sigma_term(), f)}"]
        if f is not None and f.ret is not None:
            # `while true` + `return` has no fallthrough value; keep the type.
            return ['SudoRt.fail "AssertFailed" "unreachable: missing return"']
        return [f"pure {self.build_fret(f, None)}"]

    def build_fret(self, f: Optional[Func], mret: Optional[Expr]) -> str:
        ios = [mangle_value(n) for n in self.inouts]
        if mret is not None:
            # caller already emitted mret into a term stored... we need emit here.
            # This helper expects the return *term* to be passed via payload.
            raise DecErr("internal: build_fret with expr should use build_fret_term")
        if not ios:
            return "()"
        if len(ios) == 1:
            return ios[0]
        return "(" + ", ".join(ios) + ")"

    def build_fret_term(self, ret_term: Optional[str]) -> str:
        ios = [mangle_value(n) for n in self.inouts]
        if ret_term is None:
            if not ios:
                return "()"
            if len(ios) == 1:
                return ios[0]
            return "(" + ", ".join(ios) + ")"
        if not ios:
            return ret_term
        return "(" + ", ".join([ret_term] + ios) + ")"

    def emit_return(self, mret: Optional[Expr], f: Optional[Func]) -> list[str]:
        lines: list[str] = []
        term: Optional[str] = None
        if mret is not None:
            ls, term = self.emit_expr(mret)
            lines.extend(ls)
        fret = self.build_fret_term(term)
        if self.mode == "loop":
            return lines + [f"pure {self.flow_ctor('ret', fret, f)}"]
        return lines + [f"pure {fret}"]

    def emit_block(self, stmts: list[Stmt], f: Optional[Func]) -> list[str]:
        if not stmts:
            return self.end_block(f)
        s, rest = stmts[0], stmts[1:]
        return self.emit_stmt(s, rest, f)

    def emit_stmt(self, s: Stmt, rest: list[Stmt], f: Optional[Func]) -> list[str]:
        t, p = s.tag, s.payload
        if t == "Skip":
            return self.emit_block(rest, f)
        if t == "Break":
            if self.mode != "loop":
                return ['SudoRt.fail "AssertFailed" "break outside loop"']
            return [f"pure {self.flow_ctor('brk', self.sigma_term(), f)}"]
        if t == "Continue":
            if self.mode != "loop":
                return ['SudoRt.fail "AssertFailed" "continue outside loop"']
            return [f"pure {self.flow_ctor('cont', self.sigma_term(), f)}"]
        if t == "Return":
            return self.emit_return(p, f)
        if t == "Assert":
            cond, line = p
            if cond.kind == "Binary" and cond.payload[0] == "Eq":
                ls, ns = self.bind_seq([cond.payload[1], cond.payload[2]])
                tmp = self.fresh.name("_as")
                return ls + [f"let {tmp} ← SudoRt.sudoAssertEq {ns[0]} {ns[1]} {line}"] + self.emit_block(rest, f)
            ls, c = self.emit_expr(cond)
            tmp = self.fresh.name("_as")
            return ls + [f"let {tmp} ← SudoRt.sudoAssert {c} {line}"] + self.emit_block(rest, f)
        if t == "Assign":
            target, value, _dec = p
            if value.kind == "CallFunc" and self._is_inout_call(value.payload[0]):
                return self.emit_inout_call(target, value.payload[0], value.payload[1], rest, f)
            if value.kind == "MutBuiltin":
                return self.emit_mut(target, value, rest, f)
            # Place path (base, then indices) before RHS — spec §12.
            idx_ls, target2 = self.force_place_indices(target)
            ls, v = self.emit_expr(value)
            ls2, new_root = self.emit_place_set(target2, v)
            root = place_root(target)
            return idx_ls + ls + ls2 + [f"let {mangle_value(root)} := {new_root}"] + self.emit_block(rest, f)
        if t == "TupleAssign":
            ts, _ds, value = p
            ls, v = self.emit_expr(value)
            names = [mangle_value(x) for x in ts]
            if len(names) == 1:
                bind = f"let {names[0]} := {v}"
            else:
                bind = f"let {', '.join(names)} := {v}"
                # Lean needs product destructure:
                bind = f"let ⟨{', '.join(names)}⟩ := {v}"
            return ls + [bind] + self.emit_block(rest, f)
        if t == "Expr":
            e = p
            if e.kind == "CallFunc" and self._is_inout_call(e.payload[0]):
                return self.emit_inout_call(None, e.payload[0], e.payload[1], rest, f)
            if e.kind == "MutBuiltin":
                return self.emit_mut(None, e, rest, f)
            ls, v = self.emit_expr(e)
            tmp = self.fresh.name("_u")
            return ls + [f"let {tmp} := {v}"] + self.emit_block(rest, f)
        if t == "If":
            return self.emit_if(p[0], p[1], rest, f)
        if t == "Match":
            return self.emit_match(p[0], p[1], rest, f)
        if t == "While":
            return self.emit_while(p[0], p[1], rest, f)
        if t == "ForRange":
            var, fr, to, down, body = p
            return self.emit_for_range(var, fr, to, down, body, rest, f)
        if t == "ForIn":
            vs, it, body = p
            return self.emit_for_in(vs, it, body, rest, f)
        if t == "ExpectTrap":
            return self.emit_expect_trap(p[0], p[1], p[2], rest, f)
        raise DecErr(f"unknown stmt {t}")

    def _is_inout_call(self, name: str) -> bool:
        fn = lookup_func(self.all, self.cur, name)
        return bool(fn and any(p.inout for p in fn.params))

    def emit_if(self, arms: list[tuple[Expr, list[Stmt]]], else_b: Optional[list[Stmt]], rest: list[Stmt], f: Optional[Func]) -> list[str]:
        # splice rest into each arm
        lines: list[str] = []
        pieces: list[tuple[str, list[str]]] = []
        for c, b in arms:
            ls, ct = self.emit_expr(c)
            lines.extend(ls)
            pieces.append((ct, self.emit_block(b + rest, f)))
        else_lines = self.emit_block((else_b or []) + rest, f) if else_b is not None else self.emit_block(rest, f)
        # fold ifs
        acc = else_lines
        for ct, body in reversed(pieces):
            acc = [f"if {ct} then"] + self._indent(self._ensure_do(body)) + ["else"] + self._indent(self._ensure_do(acc))
        return lines + acc

    def _ensure_do(self, lines: list[str]) -> list[str]:
        if lines and lines[0].startswith("if ") or (lines and lines[0].startswith("match ")):
            return ["do"] + self._indent(lines)
        return ["do"] + self._indent(lines)

    def _indent(self, lines: list[str]) -> list[str]:
        return ["  " + ln for ln in lines]

    def emit_match(self, scrut: Expr, arms: list[MatchArm], rest: list[Stmt], f: Optional[Func]) -> list[str]:
        # First-wins: sudo allows overlapping arms (e.g. `case 1` twice). Lean
        # rejects that as a redundant alternative, so each arm is its own
        # match with a catch-all that tries the rest.
        ls, sc = self.emit_expr(scrut)
        sc = f"({sc} : {self.render_ty(scrut.ty)})"

        def nest(remaining: list[MatchArm]) -> list[str]:
            if not remaining:
                return ['SudoRt.fail "AssertFailed" "non-exhaustive match"']
            a = remaining[0]
            more = remaining[1:]
            pat = self.emit_pat(a.pattern)
            body = self.emit_block(a.body + rest, f)
            lines = [f"match {sc} with", f"| {pat} =>"] + self._indent(self._ensure_do(body))
            if more:
                lines += ["| _ =>"] + self._indent(self._ensure_do(nest(more)))
            elif a.pattern[0] != "Wildcard":
                lines += ['| _ => SudoRt.fail "AssertFailed" "non-exhaustive match"']
            return lines

        return ls + nest(arms)

    def emit_pat(self, pat: Any) -> str:
        if pat[0] == "Int":
            return str(pat[1])
        if pat[0] == "Bool":
            return "true" if pat[1] else "false"
        if pat[0] == "Wildcard":
            return "_"
        if pat[0] == "Variant":
            _tag, en, vn, binders = pat
            bs = [mangle_value(b) for b in binders]
            if en == "Option" and vn == "Some":
                return f"some {bs[0]}" if bs else "some _"
            if en == "Option" and vn == "None":
                return "none"
            if en == "Result" and vn == "Ok":
                return f"SudoRt.SResult.ok {bs[0]}" if bs else "SudoRt.SResult.ok _"
            if en == "Result" and vn == "Err":
                return f"SudoRt.SResult.err {bs[0]}" if bs else "SudoRt.SResult.err _"
            ctor = self.qual_variant(en, vn)
            local = ctor.split(".")[-1]
            ctor_pat = "." + local
            if not bs:
                return ctor_pat
            return ctor_pat + " " + " ".join(bs)
        raise DecErr(f"unknown pattern {pat}")

    def emit_inout_call(
        self,
        target: Optional[Place],
        name: str,
        args: list[Expr],
        rest: list[Stmt],
        f: Optional[Func],
    ) -> list[str]:
        fn = lookup_func(self.all, self.cur, name)
        if fn is None:
            raise DecErr(f"unknown func {name}")
        ls, ns = self.bind_seq(args)
        call = self.emit_callee(name) + ((" " + " ".join(ns)) if ns else "")
        ios = [p for p in fn.params if p.inout]
        has_ret = fn.ret is not None
        nio = len(ios)
        tmp = self.fresh.name("_io")
        lines = ls + [f"let {tmp} ← {call}"]
        # unpack
        if has_ret and nio == 0:
            ret_term = tmp
            io_terms: list[str] = []
        elif (not has_ret) and nio == 1:
            ret_term = None
            io_terms = [tmp]
        elif (not has_ret) and nio == 0:
            ret_term = None
            io_terms = []
        else:
            # product
            names = []
            if has_ret:
                names.append(self.fresh.name("_ret"))
            names.extend(self.fresh.name(f"_iw{i}") for i in range(nio))
            lines.append(f"let ⟨{', '.join(names)}⟩ := {tmp}")
            ret_term = names[0] if has_ret else None
            io_terms = names[1:] if has_ret else names
        # write back inouts
        io_args = [a for p, a in zip(fn.params, args) if p.inout]
        for term, arg in zip(io_terms, io_args):
            root = place_root_of_expr(arg)
            rebuilt_ls, rebuilt = self.rebuild_from_expr(arg, term)
            lines.extend(rebuilt_ls)
            lines.append(f"let {mangle_value(root)} := {rebuilt}")
        if target is not None and has_ret and ret_term is not None:
            ls2, new_root = self.emit_place_set(target, ret_term)
            lines.extend(ls2)
            lines.append(f"let {mangle_value(place_root(target))} := {new_root}")
        return lines + self.emit_block(rest, f)

    def rebuild_from_expr(self, e: Expr, new_v: str) -> tuple[list[str], str]:
        if e.kind == "Local":
            return [], new_v
        if e.kind == "GetField":
            recv, name = e.payload
            if recv.ty.is_record():
                ls, r = self.emit_expr(recv) if recv.kind != "Local" else ([], mangle_value(recv.payload))
                inner = self.record_with(recv.ty.record_name(), r, name, new_v)
                return ls + self.rebuild_from_expr(recv, inner)[0], self.rebuild_from_expr(recv, inner)[1] if recv.kind != "Local" else inner
            return [], new_v
        if e.kind == "Index":
            recv, idx = e.payload
            ls1, r = self.emit_expr(recv)
            ls2, i = self.emit_expr(idx)
            tmp = self.fresh.name()
            if recv.ty.is_map():
                lines = ls1 + ls2 + [f"let {tmp} := SudoRt.mapPut {r} {i} {new_v}"]
            else:
                lines = ls1 + ls2 + [f"let {tmp} ← SudoRt.putL {r} {i} {new_v}"]
            ls3, root = self.rebuild_from_expr(recv, tmp)
            return lines + ls3, root
        return [], new_v

    def emit_mut(self, target: Optional[Place], value: Expr, rest: list[Stmt], f: Optional[Func]) -> list[str]:
        b, recv, rty, args = value.payload
        ls_r, recv_e = self.emit_place_get(recv)
        ls_a, ns = self.bind_seq(args)
        lines = ls_r + ls_a
        has_val = b in ("ListPop", "ListRemoveAt", "MapDelete", "SetAdd", "SetRemove")
        call, traps = self._mut_call(b, recv_e, rty, ns)
        tmp = self.fresh.name("_mb")
        if traps:
            lines.append(f"let {tmp} ← {call}")
        else:
            lines.append(f"let {tmp} := {call}")
        if has_val:
            new_recv = self.fresh.name("_nr")
            res = self.fresh.name("_mv")
            lines.append(f"let ⟨{new_recv}, {res}⟩ := {tmp}")
        else:
            new_recv = self.fresh.name("_nr")
            lines.append(f"let ⟨{new_recv}, _⟩ := {tmp}")
            # Unit-returning muts (sort/insert/swap/append) still bind the
            # hoist temp so a following `let _u := _hmN` is in scope.
            res = "()"
        ls2, root_term = self.emit_place_set(recv, new_recv)
        lines.extend(ls2)
        lines.append(f"let {mangle_value(place_root(recv))} := {root_term}")
        if target is not None and res is not None:
            ls3, nr = self.emit_place_set(target, res)
            lines.extend(ls3)
            lines.append(f"let {mangle_value(place_root(target))} := {nr}")
        return lines + self.emit_block(rest, f)

    def _mut_call(self, b: str, recv: str, rty: Ty, ns: list[str]) -> tuple[str, bool]:
        if b == "ListAppend":
            return f"SudoRt.appendL {recv} {ns[0]}", False
        if b == "ListPop":
            return f"SudoRt.popL {recv}", True
        if b == "ListInsert":
            return f"SudoRt.insertL {recv} {ns[0]} {ns[1]}", True
        if b == "ListRemoveAt":
            return f"SudoRt.removeAtL {recv} {ns[0]}", True
        if b == "ListSwap":
            return f"SudoRt.swapL {recv} {ns[0]} {ns[1]}", True
        if b == "ListSort":
            if rty.is_list() and rty.args[0].is_float():
                return f"SudoRt.sortFloatsL {recv}", False
            return f"SudoRt.sortL {recv}", False
        if b == "MapDelete":
            return f"SudoRt.mapDelete {recv} {ns[0]}", False
        if b == "SetAdd":
            return f"SudoRt.setAdd {recv} {ns[0]}", False
        if b == "SetRemove":
            return f"SudoRt.setRemove {recv} {ns[0]}", False
        raise DecErr(f"not a mut builtin: {b}")

    def emit_while(self, cond: Expr, body: list[Stmt], rest: list[Stmt], f: Optional[Func]) -> list[str]:
        saved_mode, saved_vars = self.mode, self.loop_vars
        vars_ = collect_threaded(self.all, self.cur, body)
        self.mode = "loop"
        self.loop_vars = vars_
        body_lines = self.emit_block(body, f)
        cond_ls, cond_t = self.emit_expr(cond)
        self.mode = saved_mode
        self.loop_vars = saved_vars
        sigma = self._sigma(vars_)
        inner_ty = self.except_flow_ty(f)
        cont_then = "pure " + self.flow_ctor("cont", sigma, f)
        go_body = cond_ls + [
            f"if !({cond_t}) then",
            f"  pure {self.flow_ctor('brk', sigma, f)}",
            "else",
            "  match ← " + self._do_as(body_lines, inner_ty) + " with",
            f"  | .ret r => pure {self.flow_ctor('ret', 'r', f)}",
            f"  | .brk s => pure {self.flow_ctor('brk', 's', f)}",
            f"  | .cont s => {self._rebind_then(vars_, 's', cont_then)}",
        ]
        after = self.emit_block(rest, f)
        step_binds = self._unbind_sigma(vars_, "σ")
        stepper = (
            "(fun σ =>\n"
            + "\n".join("    " + ln for ln in step_binds + self._ensure_do(go_body))
            + ")"
        )
        return self._run_loop(after, self._unbind_sigma(vars_, "σ"), "(2 ^ 32)", stepper, sigma, f)

    def _sigma(self, vars_: list[str]) -> str:
        vs = [mangle_value(v) for v in vars_]
        if not vs:
            return "()"
        if len(vs) == 1:
            return vs[0]
        return "(" + ", ".join(vs) + ")"

    def _unbind_sigma(self, vars_: list[str], src: str) -> list[str]:
        """Bind loop-carried names from a σ value via projections.

        Product-pattern `match` is rejected when `σ` still has metavariables
        (`fst✝`). Projections do not introduce pattern variables.
        Lean n-tuples are right-nested products.
        """
        vs = [mangle_value(v) for v in vars_]
        if not vs:
            return []
        if len(vs) == 1:
            return [] if vs[0] == src else [f"let {vs[0]} := {src}"]
        lines: list[str] = []
        cur = src
        for i, v in enumerate(vs):
            if i == len(vs) - 1:
                lines.append(f"let {v} := {cur}")
            else:
                lines.append(f"let {v} := {cur}.1")
                nxt = self.fresh.name("_sp")
                lines.append(f"let {nxt} := {cur}.2")
                cur = nxt
        return lines

    def _rebind_then(self, vars_: list[str], src: str, then: str) -> str:
        vs = [mangle_value(v) for v in vars_]
        if not vs:
            return then
        if len(vs) == 1:
            return f"(let {vs[0]} := {src}; {then})"
        binds = self._unbind_sigma(vars_, src)
        return "(" + "; ".join(binds + [then]) + ")"

    def _run_loop(
        self,
        after: list[str],
        after_binds: list[str],
        fuel: str,
        stepper: str,
        init: str,
        f: Optional[Func],
    ) -> list[str]:
        """After-loop continuation, inlined as a `runLoopOn` argument so `σ`
        is inferred from `init` before the after-function is elaborated.
        No product patterns — `after_binds` are projection `let`s."""
        rho = self.rho_str(f)
        if self.mode == "expr":
            on_ret = "fun r => pure r"
        else:
            on_ret = f"fun r => pure {self.flow_ctor('ret', 'r', f)}"
        after_fun = (
            "(fun σ =>\n"
            + "\n".join("    " + ln for ln in after_binds + self._ensure_do(after))
            + ")"
        )
        init_n = self.fresh.name("_init")
        # Bind the result so a following match-arm (`| _ =>`) cannot be
        # parsed as more of the stepper/after `do` blocks.
        return [
            f"let {init_n} := {init}",
            f"let _out ← (SudoRt.runLoopOn (ρ := {rho}) {init_n} {fuel} {stepper} {after_fun} ({on_ret}))",
            "pure _out",
        ]

    def emit_for_range(
        self,
        var: str,
        fr: Expr,
        to: Expr,
        down: bool,
        body: list[Stmt],
        rest: list[Stmt],
        f: Optional[Func],
    ) -> list[str]:
        go = self.next_go()
        saved_mode, saved_vars = self.mode, self.loop_vars
        vars_ = collect_threaded(self.all, self.cur, body)
        self.mode = "loop"
        self.loop_vars = vars_
        body_lines = self.emit_block(body, f)
        self.mode = saved_mode
        self.loop_vars = saved_vars
        ls1, from_t = self.emit_expr(fr)
        ls2, to_t = self.emit_expr(to)
        i = mangle_value(var)
        empty = f"{i} < _toV" if down else f"{i} > _toV"
        step = f"SudoRt.subI {i} (1 : Int)" if down else f"SudoRt.addI {i} (1 : Int)"
        if vars_:
            step_pat = f"({i}, {self._sigma(vars_)})"
            cont_st = f"(i', {self._sigma(vars_)})"
            init = f"(_fromV, {self._sigma(vars_)})"
        else:
            step_pat = i
            cont_st = "i'"
            init = "_fromV"
        if vars_:
            brk_from_s = f"({i}, s)"
            cont_from_s = "(i', s)"
        else:
            brk_from_s = i
            cont_from_s = "i'"
        inner_ty = self.except_flow_ty(f)
        go_body = [
            f"if {empty} then",
            f"  pure {self.flow_ctor('brk', step_pat, f)}",
            "else",
            "  match ← " + self._do_as(body_lines, inner_ty) + " with",
            f"  | .ret r => pure {self.flow_ctor('ret', 'r', f)}",
            f"  | .brk s => pure {self.flow_ctor('brk', brk_from_s, f)}",
            "  | .cont s => do",
            f"      if {i} == _toV then",
            f"        pure {self.flow_ctor('brk', brk_from_s, f)}",
            "      else do",
            f"        let i' ← {step}",
            f"        pure {self.flow_ctor('cont', cont_from_s, f)}",
        ]
        header = ls1 + ls2 + [
            f"let _fromV := {from_t}",
            f"let _toV := {to_t}",
            (
                "let fuel : Nat := "
                + (
                    "if _fromV < _toV then 1 else (_fromV - _toV).natAbs + 1"
                    if down
                    else "if _fromV > _toV then 1 else (_toV - _fromV).natAbs + 1"
                )
            ),
        ]
        after = self.emit_block(rest, f)
        if vars_:
            step_binds = [f"let {i} := σ.1"] + self._unbind_sigma(vars_, "σ.2")
            after_binds = self._unbind_sigma(vars_, "σ.2")
        else:
            step_binds = [f"let {i} := σ"]
            after_binds = []
        stepper = (
            "(fun σ =>\n"
            + "\n".join("    " + ln for ln in step_binds + self._ensure_do(go_body))
            + ")"
        )
        return header + self._run_loop(after, after_binds, "fuel", stepper, init, f)

    def emit_for_in(
        self,
        vs: list[str],
        it: Expr,
        body: list[Stmt],
        rest: list[Stmt],
        f: Optional[Func],
    ) -> list[str]:
        go = self.next_go()
        saved_mode, saved_vars = self.mode, self.loop_vars
        vars_ = collect_threaded(self.all, self.cur, body)
        self.mode = "loop"
        self.loop_vars = vars_
        body_lines = self.emit_block(body, f)
        self.mode = saved_mode
        self.loop_vars = saved_vars
        ls, it_t = self.emit_expr(it)
        if it.ty.is_set():
            snap = f"SudoRt.setItemsL {it_t}"
        elif it.ty.is_map():
            snap = f"SudoRt.mapKeysL {it_t}"  # overwritten below
        else:
            snap = it_t
        if it.ty.is_map():
            # iterate (k, v) pairs
            snap = f"(Array.mk ({it_t}).entries)"
        header = ls + [f"let _items := {snap}", "let fuel : Nat := _items.size + 1"]
        binders: str
        if it.ty.is_map() and len(vs) == 2:
            binders = f"let {mangle_value(vs[0])} := _hd.1; let {mangle_value(vs[1])} := _hd.2"
        elif len(vs) == 1:
            binders = f"let {mangle_value(vs[0])} := _hd"
        else:
            binders = "let ⟨" + ", ".join(mangle_value(v) for v in vs) + "⟩ := _hd"
        if vars_:
            step_pat = f"(_remaining, {self._sigma(vars_)})"
            cont_st = f"(_tl, {self._sigma(vars_)})"
            init = f"(_items, {self._sigma(vars_)})"
            after_pat = f"(_, {self._sigma(vars_)})"
        else:
            step_pat = "_remaining"
            cont_st = "_tl"
            init = "_items"
            after_pat = "_"
        if vars_:
            brk_from_s = "(_remaining, s)"
            cont_from_s = "(_tl, s)"
        else:
            brk_from_s = "_remaining"
            cont_from_s = "_tl"
        inner_ty = self.except_flow_ty(f)
        go_body = [
            "if _remaining.size == 0 then",
            f"  pure {self.flow_ctor('brk', step_pat, f)}",
            "else do",
            "  let _hd := _remaining[0]!",
            "  let _tl := _remaining.extract 1 _remaining.size",
            f"  {binders}",
            "  match ← " + self._do_as(body_lines, inner_ty) + " with",
            f"  | .ret r => pure {self.flow_ctor('ret', 'r', f)}",
            f"  | .brk s => pure {self.flow_ctor('brk', brk_from_s, f)}",
            f"  | .cont s => pure {self.flow_ctor('cont', cont_from_s, f)}",
        ]
        after = self.emit_block(rest, f)
        if vars_:
            step_binds = ["let _remaining := σ.1"] + self._unbind_sigma(vars_, "σ.2")
            after_binds = self._unbind_sigma(vars_, "σ.2")
        else:
            step_binds = ["let _remaining := σ"]
            after_binds = []
        stepper = (
            "(fun σ =>\n"
            + "\n".join("    " + ln for ln in step_binds + self._ensure_do(go_body))
            + ")"
        )
        return header + self._run_loop(after, after_binds, "fuel", stepper, init, f)

    def emit_expect_trap(self, kind: str, body: list[Stmt], line: int, rest: list[Stmt], f: Optional[Func]) -> list[str]:
        saved = self.mode
        self.mode = "expr"
        saved_io = self.inouts
        self.inouts = []
        body_lines = self.emit_block(body, None)
        self.mode = saved
        self.inouts = saved_io
        tmp = self.fresh.name("_ex")
        lines = [
            f"let {tmp} := " + self._do(body_lines + ["pure ()"]),
            f"match {tmp} with",
            f'| .error t => if t.kind == "{kind}" then pure () else '
            f'SudoRt.fail "AssertFailed" s!"line {line}: expected trap {kind}, got {{t.kind}}"',
            f'| .ok _ => SudoRt.fail "AssertFailed" "line {line}: expected trap {kind}, but nothing trapped"',
        ]
        return lines + self.emit_block(rest, f)

    def emit_func(self, f: Func, *, as_go: bool = False) -> list[str]:
        saved_fuel = self.emitting_fueled
        self.emitting_fueled = as_go
        self.mode = "expr"
        self.inouts = [p.name for p in f.params if p.inout]
        self.loop_vars = []
        self.go_n = 0
        body = hoist_stmts(f.body, self.fresh)
        body_lines = self.emit_block(body, f)
        self.emitting_fueled = saved_fuel
        fn = mangle_value(f.name) + ("_go" if as_go else "")
        params = " ".join(f"({mangle_value(p.name)} : {self.render_ty(p.ty)})" for p in f.params)
        ret = f"Except SudoRt.Trap ({self.fret_ty_str(f)})"
        if as_go:
            sig = f"def {fn} (_rfuel : Nat) {params} : {ret} :=".strip()
            inner = [
                "match _rfuel with",
                '  | 0 => SudoRt.fail "StackOverflow" "recursive fuel exhausted"',
                "  | _rfuel + 1 =>",
            ] + self._indent(self._ensure_do(body_lines))
            return [sig] + self._indent(inner) + [""]
        sig = f"def {fn} {params} : {ret} :=".strip()
        if not params:
            sig = f"def {fn} : {ret} :="
        return [sig] + self._indent(self._ensure_do(body_lines)) + [""]

    def emit_func_wrapper(self, f: Func) -> list[str]:
        fn = mangle_value(f.name)
        params = " ".join(f"({mangle_value(p.name)} : {self.render_ty(p.ty)})" for p in f.params)
        args = " ".join(mangle_value(p.name) for p in f.params)
        ret = f"Except SudoRt.Trap ({self.fret_ty_str(f)})"
        sig = f"def {fn} {params} : {ret} :=".strip()
        if not params:
            sig = f"def {fn} : {ret} :="
        return [sig, f"  {fn}_go (2 ^ 32) {args}".rstrip(), ""]

    def emit_const(self, c: Const) -> list[str]:
        # Consts are folded literals; emit as a non-monadic value.
        saved = self.mode
        self.mode = "expr"
        ls, term = self.emit_expr(c.value)
        self.mode = saved
        if ls:
            # Should not happen for a true constant; wrap in Id.run / panic.
            body = self._do(ls + [f"pure {term}"])
            return [
                f"def {mangle_value(c.name)} : {self.render_ty(c.ty)} :=",
                f"  match {body} with",
                f"  | .ok v => v",
                f'  | .error _ => panic! "sudo const {c.name} trapped"',
                "",
            ]
        return [
            f"def {mangle_value(c.name)} : {self.render_ty(c.ty)} := {term}",
            "",
        ]

    def emit_record_decl(self, r: Record) -> list[str]:
        tn = mangle_type(r.name)
        lines = [f"structure {tn} where"]
        for fn, ty in r.fields:
            lines.append(f"  {mangle_field(r.name, fn)} : {self.render_ty(ty)}")
        lines.append("")
        return lines

    def emit_record_inductive(self, r: Record) -> list[str]:
        tn = mangle_type(r.name)
        lines = [f"inductive {tn} : Type where"]
        if not r.fields:
            lines.append("  | mk")
        else:
            bits = " ".join(
                f"({mangle_field(r.name, fn)} : {self.render_ty(ty)})" for fn, ty in r.fields
            )
            lines.append(f"  | mk {bits}")
        lines.append("  deriving BEq, Repr")
        lines.append("")
        return lines

    def emit_record_projections(self, r: Record) -> list[str]:
        tn = mangle_type(r.name)
        lines: list[str] = []
        for i, (fn, ty) in enumerate(r.fields):
            fld = mangle_field(r.name, fn)
            pats = " ".join(f"x{j}" if j == i else "_" for j in range(len(r.fields)))
            lines.append(f"def {tn}.{fld} : {tn} → {self.render_ty(ty)}")
            lines.append(f"  | .mk {pats} => x{i}")
        if lines:
            lines.append("")
        return lines

    def record_with(self, rec_name: str, base: str, field: str, val: str) -> str:
        rec = next((r for r in self._all_records() if r.name == rec_name), None)
        if rec is None or rec_name not in self.inductive_records:
            fld = mangle_field(rec_name, field)
            return f"{{ {base} with {fld} := {val} }}"
        args: list[str] = []
        for fn, _ in rec.fields:
            fld = mangle_field(rec_name, fn)
            args.append(val if fn == field else f"({base}).{fld}")
        return f"({self.qual_type(rec_name)}.mk {' '.join(args)})"

    def emit_enum_decl(self, e: Enum) -> list[str]:
        tn = mangle_type(e.name)
        lines = [f"inductive {tn} : Type where"]
        for vn, fields in e.variants:
            ctor = mangle_variant(e.name, vn)
            if not fields:
                lines.append(f"  | {ctor}")
            else:
                bits = " ".join(
                    f"({mangle_field(e.name + '.' + vn, fn)} : {self.render_ty(ty)})" for fn, ty in fields
                )
                lines.append(f"  | {ctor} {bits}")
        lines.append("  deriving BEq, Repr")
        lines.append("")
        return lines

    def emit_record_inhabited(self, r: Record) -> list[str]:
        tn = mangle_type(r.name)
        if r.name in self.inductive_records:
            if not r.fields:
                rhs = f"{tn}.mk"
            else:
                rhs = f"{tn}.mk " + " ".join("default" for _ in r.fields)
            return [f"instance : Inhabited {tn} where", f"  default := {rhs}", ""]
        if not r.fields:
            return [f"instance : Inhabited {tn} where", "  default := {}", ""]
        fields = ", ".join(f"{mangle_field(r.name, fn)} := default" for fn, _ in r.fields)
        return [f"instance : Inhabited {tn} where", f"  default := {{ {fields} }}", ""]

    def emit_enum_inhabited(self, e: Enum) -> list[str]:
        tn = mangle_type(e.name)
        # Prefer a constructor whose payload does not mention another
        # module-local nominal, so `default` is not a recursive knot.
        local_noms = {x.name for x in self.cur.records} | {x.name for x in self.cur.enums}

        def mentions_local(ty: Ty) -> bool:
            if ty.tag in ("Record", "Enum"):
                return ty.args[0] in local_noms
            if ty.tag in ("List", "Set", "Option"):
                return mentions_local(ty.args[0])
            if ty.tag == "Map":
                return mentions_local(ty.args[0]) or mentions_local(ty.args[1])
            if ty.tag == "Result":
                return mentions_local(ty.args[0]) or mentions_local(ty.args[1])
            if ty.tag == "Tuple":
                return any(mentions_local(a) for a in ty.args)
            return False

        chosen = e.variants[0]
        for vn, fields in e.variants:
            if not any(mentions_local(ty) for _, ty in fields):
                chosen = (vn, fields)
                break
        ctor = mangle_variant(e.name, chosen[0])
        if not chosen[1]:
            rhs = f"{tn}.{ctor}"
        else:
            rhs = f"{tn}.{ctor} " + " ".join("default" for _ in chosen[1])
        return [f"instance : Inhabited {tn} where", f"  default := {rhs}", ""]

    def emit_record_seq(self, r: Record) -> list[str]:
        tn = mangle_type(r.name)
        lines = [f"instance : SudoRt.SEq {tn} where"]
        if not r.fields:
            lines.append("  beq _ _ := true")
        else:
            conds = [
                f"SudoRt.SEq.beq a.{mangle_field(r.name, fn)} b.{mangle_field(r.name, fn)}"
                for fn, _ in r.fields
            ]
            lines.append("  beq a b := " + " && ".join(conds))
        lines.append("")
        return lines

    def emit_record_ord(self, r: Record) -> list[str]:
        tn = mangle_type(r.name)
        lines = [f"instance : SudoRt.SOrd {tn} where"]
        if not r.fields:
            lines.append("  le _ _ := true")
        else:
            lines.append("  le a b :=")
            acc = "true"
            for fn, _ in reversed(r.fields):
                fld = mangle_field(r.name, fn)
                acc = (
                    f"(if !(SudoRt.SEq.beq a.{fld} b.{fld}) then SudoRt.SOrd.le a.{fld} b.{fld} else {acc})"
                )
            lines.append("    " + acc)
        lines.append("")
        return lines

    def emit_record_canon(self, r: Record) -> list[str]:
        tn = mangle_type(r.name)
        lines = [f"instance : SudoRt.Canon {tn} where"]
        canons = [f"SudoRt.Canon.canon r.{mangle_field(r.name, fn)}" for fn, _ in r.fields]
        if canons:
            lines.append(f'  canon r := SudoRt.canonRecord "{r.name}" [{", ".join(canons)}]')
        else:
            lines.append(f'  canon _ := SudoRt.canonRecord "{r.name}" []')
        lines.append("")
        return lines

    def emit_enum_seq(self, e: Enum) -> list[str]:
        tn = mangle_type(e.name)
        lines = [f"instance : SudoRt.SEq {tn} where", "  beq"]
        for vn, fields in e.variants:
            ctor = mangle_variant(e.name, vn)
            if not fields:
                lines.append(f"    | .{ctor}, .{ctor} => true")
            else:
                as_ = " ".join(f"a{i}" for i, _ in enumerate(fields))
                bs_ = " ".join(f"b{i}" for i, _ in enumerate(fields))
                conds = [f"SudoRt.SEq.beq a{i} b{i}" for i, _ in enumerate(fields)]
                lines.append(f"    | .{ctor} {as_}, .{ctor} {bs_} => " + " && ".join(conds))
        if len(e.variants) > 1:
            lines.append("    | _, _ => false")
        lines.append("")
        return lines

    def emit_enum_ord(self, e: Enum) -> list[str]:
        tn = mangle_type(e.name)
        lines = [f"instance : SudoRt.SOrd {tn} where", "  le a b :="]
        lines.append(f"    let idx : {tn} → Nat := fun")
        for i, (vn, _fields) in enumerate(e.variants):
            ctor = mangle_variant(e.name, vn)
            lines.append(f"      | .{ctor} .. => {i}")
        lines.append("    let ia := idx a; let ib := idx b")
        lines.append("    if ia != ib then decide (ia ≤ ib) else")
        lines.append("    match a, b with")
        for vn, fields in e.variants:
            ctor = mangle_variant(e.name, vn)
            if not fields:
                lines.append(f"    | .{ctor}, .{ctor} => true")
            else:
                as_ = " ".join(f"a{i}" for i, _ in enumerate(fields))
                bs_ = " ".join(f"b{i}" for i, _ in enumerate(fields))
                acc = "true"
                for i in range(len(fields) - 1, -1, -1):
                    acc = (
                        f"(if !(SudoRt.SEq.beq a{i} b{i}) then SudoRt.SOrd.le a{i} b{i} else {acc})"
                    )
                lines.append(f"    | .{ctor} {as_}, .{ctor} {bs_} => {acc}")
        if len(e.variants) > 1:
            lines.append("    | _, _ => true")
        lines.append("")
        return lines

    def _scc_beq(self, t: Ty, a: str, b: str, scc: set[str]) -> str:
        if t.tag in ("Record", "Enum") and t.args[0] in scc:
            return f"{mangle_type(t.args[0])}_beq {a} {b}"
        if t.tag == "List":
            return f"SudoRt.beqBy {self._scc_beq_fn(t.args[0], scc)} {a} {b}"
        return f"SudoRt.SEq.beq {a} {b}"

    def _scc_beq_fn(self, t: Ty, scc: set[str]) -> str:
        if t.tag in ("Record", "Enum") and t.args[0] in scc:
            return f"{mangle_type(t.args[0])}_beq"
        if t.tag == "List":
            return f"(SudoRt.beqBy {self._scc_beq_fn(t.args[0], scc)})"
        return "SudoRt.SEq.beq"

    def _scc_le(self, t: Ty, a: str, b: str, scc: set[str]) -> str:
        if t.tag in ("Record", "Enum") and t.args[0] in scc:
            return f"{mangle_type(t.args[0])}_le {a} {b}"
        if t.tag == "List":
            return (
                f"SudoRt.leBy {self._scc_beq_fn(t.args[0], scc)} "
                f"{self._scc_le_fn(t.args[0], scc)} {a} {b}"
            )
        return f"SudoRt.SOrd.le {a} {b}"

    def _scc_le_fn(self, t: Ty, scc: set[str]) -> str:
        if t.tag in ("Record", "Enum") and t.args[0] in scc:
            return f"{mangle_type(t.args[0])}_le"
        if t.tag == "List":
            return (
                f"(SudoRt.leBy {self._scc_beq_fn(t.args[0], scc)} "
                f"{self._scc_le_fn(t.args[0], scc)})"
            )
        return "SudoRt.SOrd.le"

    def _scc_canon(self, t: Ty, a: str, scc: set[str]) -> str:
        if t.tag in ("Record", "Enum") and t.args[0] in scc:
            return f"{mangle_type(t.args[0])}_canon {a}"
        if t.tag == "List":
            return f"SudoRt.canonBy {self._scc_canon_fn(t.args[0], scc)} {a}"
        return f"SudoRt.Canon.canon {a}"

    def _scc_canon_fn(self, t: Ty, scc: set[str]) -> str:
        if t.tag in ("Record", "Enum") and t.args[0] in scc:
            return f"{mangle_type(t.args[0])}_canon"
        if t.tag == "List":
            return f"(SudoRt.canonBy {self._scc_canon_fn(t.args[0], scc)})"
        return "SudoRt.Canon.canon"

    def emit_cyclic_seq(self, recs: list[Record], ens: list[Enum], scc: set[str]) -> list[str]:
        lines = ["mutual"]
        for r in recs:
            tn = mangle_type(r.name)
            if not r.fields:
                lines += [f"def {tn}_beq (a b : {tn}) : Bool :=", "  true", ""]
            else:
                as_ = " ".join(f"a{i}" for i in range(len(r.fields)))
                bs_ = " ".join(f"b{i}" for i in range(len(r.fields)))
                conds = [
                    self._scc_beq(ty, f"a{i}", f"b{i}", scc) for i, (_, ty) in enumerate(r.fields)
                ]
                lines += [
                    f"def {tn}_beq (a b : {tn}) : Bool :=",
                    "  match a, b with",
                    f"    | .mk {as_}, .mk {bs_} => " + " && ".join(conds),
                    "",
                ]
        for e in ens:
            tn = mangle_type(e.name)
            lines += [f"def {tn}_beq (a b : {tn}) : Bool :=", "  match a, b with"]
            for vn, fields in e.variants:
                ctor = mangle_variant(e.name, vn)
                if not fields:
                    lines.append(f"    | .{ctor}, .{ctor} => true")
                else:
                    as_ = " ".join(f"a{i}" for i, _ in enumerate(fields))
                    bs_ = " ".join(f"b{i}" for i, _ in enumerate(fields))
                    conds = [self._scc_beq(ty, f"a{i}", f"b{i}", scc) for i, (_, ty) in enumerate(fields)]
                    lines.append(f"    | .{ctor} {as_}, .{ctor} {bs_} => " + " && ".join(conds))
            if len(e.variants) > 1:
                lines.append("    | _, _ => false")
            lines.append("")
        lines.append("end")
        lines.append("")
        for r in recs:
            tn = mangle_type(r.name)
            lines += [f"instance : SudoRt.SEq {tn} where", f"  beq := {tn}_beq", ""]
        for e in ens:
            tn = mangle_type(e.name)
            lines += [f"instance : SudoRt.SEq {tn} where", f"  beq := {tn}_beq", ""]
        return lines

    def emit_cyclic_ord(self, recs: list[Record], ens: list[Enum], scc: set[str]) -> list[str]:
        lines = ["mutual"]
        for r in recs:
            tn = mangle_type(r.name)
            if not r.fields:
                lines += [f"def {tn}_le (a b : {tn}) : Bool :=", "  true", ""]
            else:
                as_ = " ".join(f"a{i}" for i in range(len(r.fields)))
                bs_ = " ".join(f"b{i}" for i in range(len(r.fields)))
                acc = "true"
                for i, (_, ty) in reversed(list(enumerate(r.fields))):
                    eq = self._scc_beq(ty, f"a{i}", f"b{i}", scc)
                    le = self._scc_le(ty, f"a{i}", f"b{i}", scc)
                    acc = f"(if !({eq}) then {le} else {acc})"
                lines += [
                    f"def {tn}_le (a b : {tn}) : Bool :=",
                    "  match a, b with",
                    f"    | .mk {as_}, .mk {bs_} => {acc}",
                    "",
                ]
        for e in ens:
            tn = mangle_type(e.name)
            lines += [f"def {tn}_le (a b : {tn}) : Bool :=", f"  let idx : {tn} → Nat := fun"]
            for i, (vn, _fields) in enumerate(e.variants):
                ctor = mangle_variant(e.name, vn)
                lines.append(f"    | .{ctor} .. => {i}")
            lines.append("  let ia := idx a; let ib := idx b")
            lines.append("  if ia != ib then decide (ia ≤ ib) else")
            lines.append("  match a, b with")
            for vn, fields in e.variants:
                ctor = mangle_variant(e.name, vn)
                if not fields:
                    lines.append(f"    | .{ctor}, .{ctor} => true")
                else:
                    as_ = " ".join(f"a{i}" for i, _ in enumerate(fields))
                    bs_ = " ".join(f"b{i}" for i, _ in enumerate(fields))
                    acc = "true"
                    for i, (_, ty) in reversed(list(enumerate(fields))):
                        eq = self._scc_beq(ty, f"a{i}", f"b{i}", scc)
                        le = self._scc_le(ty, f"a{i}", f"b{i}", scc)
                        acc = f"(if !({eq}) then {le} else {acc})"
                    lines.append(f"    | .{ctor} {as_}, .{ctor} {bs_} => {acc}")
            if len(e.variants) > 1:
                lines.append("    | _, _ => true")
            lines.append("")
        lines.append("end")
        lines.append("")
        for r in recs:
            tn = mangle_type(r.name)
            lines += [f"instance : SudoRt.SOrd {tn} where", f"  le := {tn}_le", ""]
        for e in ens:
            tn = mangle_type(e.name)
            lines += [f"instance : SudoRt.SOrd {tn} where", f"  le := {tn}_le", ""]
        return lines

    def emit_cyclic_canon(self, recs: list[Record], ens: list[Enum], scc: set[str]) -> list[str]:
        lines = ["mutual"]
        for r in recs:
            tn = mangle_type(r.name)
            if not r.fields:
                lines += [
                    f"def {tn}_canon (r : {tn}) : String :=",
                    f'  SudoRt.canonRecord "{r.name}" []',
                    "",
                ]
            else:
                as_ = " ".join(f"a{i}" for i in range(len(r.fields)))
                canons = ", ".join(
                    self._scc_canon(ty, f"a{i}", scc) for i, (_, ty) in enumerate(r.fields)
                )
                lines += [
                    f"def {tn}_canon (r : {tn}) : String :=",
                    "  match r with",
                    f'    | .mk {as_} => SudoRt.canonRecord "{r.name}" [{canons}]',
                    "",
                ]
        for e in ens:
            tn = mangle_type(e.name)
            lines += [f"def {tn}_canon (a : {tn}) : String :=", "  match a with"]
            for vn, fields in e.variants:
                ctor = mangle_variant(e.name, vn)
                if not fields:
                    lines.append(f'    | .{ctor} => SudoRt.canonEnum "{e.name}" "{vn}" []')
                else:
                    as_ = " ".join(f"a{i}" for i, _ in enumerate(fields))
                    canons = ", ".join(
                        self._scc_canon(ty, f"a{i}", scc) for i, (_, ty) in enumerate(fields)
                    )
                    lines.append(f'    | .{ctor} {as_} => SudoRt.canonEnum "{e.name}" "{vn}" [{canons}]')
            lines.append("")
        lines.append("end")
        lines.append("")
        for r in recs:
            tn = mangle_type(r.name)
            lines += [f"instance : SudoRt.Canon {tn} where", f"  canon := {tn}_canon", ""]
        for e in ens:
            tn = mangle_type(e.name)
            lines += [f"instance : SudoRt.Canon {tn} where", f"  canon := {tn}_canon", ""]
        return lines

    def emit_enum_canon(self, e: Enum) -> list[str]:
        tn = mangle_type(e.name)
        lines = [f"instance : SudoRt.Canon {tn} where", "  canon"]
        for vn, fields in e.variants:
            ctor = mangle_variant(e.name, vn)
            if not fields:
                lines.append(f'    | .{ctor} => SudoRt.canonEnum "{e.name}" "{vn}" []')
            else:
                as_ = " ".join(f"a{i}" for i, _ in enumerate(fields))
                canons = ", ".join(f"SudoRt.Canon.canon a{i}" for i, _ in enumerate(fields))
                lines.append(f'    | .{ctor} {as_} => SudoRt.canonEnum "{e.name}" "{vn}" [{canons}]')
        lines.append("")
        return lines

    def emit_module_src(self) -> str:
        m = self.cur
        lines = [
            f"-- Generated by sudoc lean backend from {m.name}.sudo",
            "import SudoRt",
        ]
        for imp in m.imports:
            if imp != m.name:
                lines.append(f"import {mangle_module(imp)}")
        lines.append("set_option linter.unusedVariables false")
        lines.append("")
        ns = mangle_module(m.name)
        lines.append(f"namespace {ns}")
        lines.append("")
        # One mutual block so records/enums may forward-ref and recurse
        # (regex Item ↔ Atom, CompiledPattern → NfaState). Instances come
        # after the types exist; they are themselves mutual so SEq Item
        # can mention SEq Atom and vice versa.
        rec_by = {r.name: r for r in m.records}
        en_by = {e.name: e for e in m.enums}
        for scc in module_type_sccs(m):
            recs = [rec_by[n] for n in scc if n in rec_by]
            ens = [en_by[n] for n in scc if n in en_by]
            cyclic = len(scc) > 1
            if cyclic:
                # Lean 4.14 forbids mixing `structure` and `inductive` in one
                # mutual block. Encode the records as inductives + projections.
                lines.append("mutual")
                for r in recs:
                    lines.extend(self.emit_record_inductive(r))
                for e in ens:
                    lines.extend(self.emit_enum_decl(e))
                lines.append("end")
                lines.append("")
                for r in recs:
                    lines.extend(self.emit_record_projections(r))
                # Inhabited: non-recursive enum ctors first, then records.
                for e in ens:
                    lines.extend(self.emit_enum_inhabited(e))
                for r in recs:
                    lines.extend(self.emit_record_inhabited(r))
                for r in recs:
                    tn = mangle_type(r.name)
                    lines += [
                        f"instance : SudoRt.SEq {tn} where",
                        "  beq a b := decide (a == b)",
                        f"instance : SudoRt.SOrd {tn} where",
                        "  le _ _ := true",
                        f"instance : SudoRt.Canon {tn} where",
                        "  canon a := toString (repr a)",
                        "",
                    ]
                for e in ens:
                    tn = mangle_type(e.name)
                    lines += [
                        f"instance : SudoRt.SEq {tn} where",
                        "  beq a b := decide (a == b)",
                        f"instance : SudoRt.SOrd {tn} where",
                        "  le _ _ := true",
                        f"instance : SudoRt.Canon {tn} where",
                        "  canon a := toString (repr a)",
                        "",
                    ]
            else:
                for r in recs:
                    lines.extend(self.emit_record_decl(r))
                    lines.extend(self.emit_record_inhabited(r))
                    lines.extend(self.emit_record_seq(r))
                    lines.extend(self.emit_record_ord(r))
                    lines.extend(self.emit_record_canon(r))
                for e in ens:
                    lines.extend(self.emit_enum_decl(e))
                    lines.extend(self.emit_enum_inhabited(e))
                    lines.extend(self.emit_enum_seq(e))
                    lines.extend(self.emit_enum_ord(e))
                    lines.extend(self.emit_enum_canon(e))
        for c in m.consts:
            lines.extend(self.emit_const(c))
        if m.funcs:
            by_name = {f.name: f for f in m.funcs}
            sccs, _fdeps = module_func_sccs(m)
            for scc in sccs:
                rec = any(n in self.fueled for n in scc)
                fns = [by_name[n] for n in scc]
                saved_scc = self.current_scc
                self.current_scc = set(scc)
                if rec:
                    lines.append("mutual")
                    for f in fns:
                        lines.extend(self.emit_func(f, as_go=True))
                    lines.append("end")
                    lines.append("")
                    for f in fns:
                        lines.extend(self.emit_func_wrapper(f))
                elif len(fns) > 1:
                    lines.append("mutual")
                    for f in fns:
                        lines.extend(self.emit_func(f))
                    lines.append("end")
                    lines.append("")
                else:
                    for f in fns:
                        lines.extend(self.emit_func(f))
                self.current_scc = saved_scc
        # An empty module still needs a declaration so `open Mod` is legal.
        if not (m.records or m.enums or m.consts or m.funcs):
            lines.append("def _sudo_unit : Unit := ()")
            lines.append("")
        lines.append(f"end {ns}")
        return "\n".join(lines).rstrip() + "\n"

    def emit_test_fn(self, fn: str, t: Test) -> list[str]:
        self.mode = "expr"
        self.inouts = []
        self.loop_vars = []
        self.go_n = 0
        body = hoist_stmts(t.body, self.fresh)
        body_lines = self.emit_block(body, None)
        return [
            f"def {fn} : Except SudoRt.Trap Unit :=",
        ] + self._indent(self._ensure_do(body_lines)) + [""]

    def emit_test_file(self) -> str:
        m = self.cur
        names = test_fn_names(m.tests)
        lines = [
            f"-- Generated tests for {m.name}.sudo",
            "import SudoRt",
            f"import {mangle_module(m.name)}",
        ]
        for imp in m.imports:
            lines.append(f"import {mangle_module(imp)}")
        lines.append("set_option linter.unusedVariables false")
        lines.append(f"open {mangle_module(m.name)}")
        lines.append("")
        for fn, t in zip(names, m.tests):
            lines.extend(self.emit_test_fn(fn, t))
        entries = ", ".join(f'("{fn}", fun _ => {fn})' for fn in names)
        lines += [
            "def main : IO UInt32 :=",
            f"  SudoRt.runTests [{entries}]",
            "",
        ]
        return "\n".join(lines)


def emit_lakefile(mods: list[Module], entry: str) -> str:
    libs = ["SudoRt"] + [mangle_module(m.name) for m in mods]
    lib_lines = "\n".join(f"lean_lib {lib}" for lib in libs)
    exe = f"{entry}_test"
    return (
        "import Lake\n"
        "open Lake DSL\n"
        "\n"
        "package sudo\n"
        "\n"
        f"{lib_lines}\n"
        "\n"
        f"@[default_target]\n"
        f"lean_exe {exe} where\n"
        f"  root := `{exe}\n"
    )


def emit_all(runtime_src: str, req: EmitReq) -> list[tuple[str, str]]:
    files: list[tuple[str, str]] = [("SudoRt.lean", runtime_src)]
    files.append(("lean-toolchain", "leanprover/lean4:v4.14.0\n"))
    files.append(("lakefile.lean", emit_lakefile(req.modules, req.entry)))
    for m in req.modules:
        em = Em(req.modules, m)
        files.append((mangle_module(m.name) + ".lean", em.emit_module_src()))
    if req.with_tests:
        entry = req.modules[-1]
        em = Em(req.modules, entry)
        files.append((f"{req.entry}_test.lean", em.emit_test_file()))
    return files


def respond_error(msg: str) -> None:
    sys.stdout.write(json.dumps({"error": msg}, ensure_ascii=False))
    sys.stdout.write("\n")


def respond_ok(files: list[tuple[str, str]]) -> None:
    payload = {"files": [{"path": p, "contents": c} for p, c in files]}
    sys.stdout.write(json.dumps(payload, ensure_ascii=False))
    sys.stdout.write("\n")


def main() -> None:
    raw = sys.stdin.read()
    try:
        val = json.loads(raw)
    except json.JSONDecodeError as e:
        respond_error(f"JSON parse error: {e}")
        return
    try:
        req = decode_request(val)
    except DecErr as e:
        respond_error(f"IR decode error: {e}")
        return
    try:
        with open("SudoRt.lean", encoding="utf-8") as fh:
            runtime_src = fh.read()
        files = emit_all(runtime_src, req)
    except DecErr as e:
        respond_error(str(e))
        return
    except Exception as e:  # noqa: BLE001 — surface any emit bug as a protocol error
        respond_error(f"emit error: {type(e).__name__}: {e}")
        return
    respond_ok(files)


if __name__ == "__main__":
    main()
