#!/usr/bin/env python3
"""sudo → Lean 4 external-backend emitter (protocol 4).

Reads one emit-request envelope on stdin; writes one {"files":[...]} or
{"error": "..."} response on stdout. Parse is strict: unknown protocol
versions, unknown fields, and unknown IR tags are errors.

While loops and recursive functions are refused (decreases is erased from
the IR, so they cannot be emitted as Lean total defs). for-range / for-in
compile to Nat-fueled SudoRt helpers. The frontend `terminates` predicate
is applied by sudoc before this process runs — this file does not read it.
"""

from __future__ import annotations

import json
import math
import os
import sys
from dataclasses import dataclass, field
from typing import Any, Callable, Optional, Union


# ---------------------------------------------------------------------------
# Strict JSON helpers (protocol.md §3)
# ---------------------------------------------------------------------------

class DecodeError(Exception):
    pass


def expect_obj(v: Any, what: str) -> dict:
    if not isinstance(v, dict):
        raise DecodeError(f"{what}: expected object")
    return v


def expect_keys(v: Any, allowed: list[str], what: str) -> dict:
    obj = expect_obj(v, what)
    extra = [k for k in obj if k not in allowed]
    if extra:
        raise DecodeError(f"{what}: unknown fields: {extra}")
    missing = [k for k in allowed if k not in obj]
    if missing:
        raise DecodeError(f"{what}: missing fields: {missing}")
    return obj


def as_str(v: Any, what: str) -> str:
    if not isinstance(v, str):
        raise DecodeError(f"{what}: expected string")
    return v


def as_bool(v: Any, what: str) -> bool:
    if not isinstance(v, bool):
        raise DecodeError(f"{what}: expected bool")
    return v


def as_arr(v: Any, what: str) -> list:
    if not isinstance(v, list):
        raise DecodeError(f"{what}: expected array")
    return v


def as_i64(v: Any, what: str) -> int:
    if not isinstance(v, str):
        raise DecodeError(f"{what}: expected i64 decimal string")
    try:
        n = int(v, 10)
    except ValueError as e:
        raise DecodeError(f"{what}: invalid i64 string: {v!r}") from e
    if n < -(1 << 63) or n > (1 << 63) - 1:
        raise DecodeError(f"{what}: i64 out of range: {v}")
    if (v.startswith("-") and str(n) != v) or (not v.startswith("-") and str(n) != v):
        # reject leading zeros / plus signs except the canonical form; "-0" → 0
        if v in ("0", "-0") and n == 0:
            return 0
        if str(n) != v:
            raise DecodeError(f"{what}: invalid i64 string: {v!r}")
    return n


def as_i64_num(v: Any, what: str) -> int:
    if isinstance(v, bool) or not isinstance(v, int):
        raise DecodeError(f"{what}: expected integer number")
    if v < -(1 << 63) or v > (1 << 63) - 1:
        raise DecodeError(f"{what}: i64 out of range: {v}")
    return v


def as_float(v: Any, what: str) -> float:
    if isinstance(v, str):
        if v == "nan":
            return math.nan
        if v == "inf":
            return math.inf
        if v == "-inf":
            return -math.inf
        raise DecodeError(f"{what}: unknown float string: {v!r}")
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        raise DecodeError(f"{what}: expected float number or nan/inf/-inf string")
    return float(v)


def as_line(v: Any, what: str) -> int:
    if isinstance(v, bool) or not isinstance(v, int):
        raise DecodeError(f"{what}: line must be number")
    return v


def ext_tag(v: Any, what: str) -> tuple[str, Any]:
    if isinstance(v, str):
        return v, None
    if isinstance(v, dict):
        if len(v) != 1:
            raise DecodeError(f"{what}: expected single-key object, got keys {list(v)}")
        k, val = next(iter(v.items()))
        return k, val
    raise DecodeError(f"{what}: expected string or single-key object")


# ---------------------------------------------------------------------------
# IR
# ---------------------------------------------------------------------------

Ty = Any  # nested tuples/dicts; see decode_ty


@dataclass
class Place:
    kind: str
    name: str = ""
    base: Optional["Place"] = None
    base_ty: Any = None
    index: Any = None
    field: str = ""


@dataclass
class Expr:
    ty: Any
    kind: str
    payload: Any = None


@dataclass
class Stmt:
    kind: str
    payload: Any = None


@dataclass
class Param:
    name: str
    inout: bool
    ty: Any
    never_written: bool


@dataclass
class Func:
    name: str
    export: bool
    params: list[Param]
    ret: Any
    body: list[Stmt]


@dataclass
class Test:
    name: str
    body: list[Stmt]


@dataclass
class Record:
    name: str
    fields: list[tuple[str, Any]]


@dataclass
class Enum:
    name: str
    variants: list[tuple[str, list[tuple[str, Any]]]]


@dataclass
class Const:
    name: str
    ty: Any
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


def decode_ty(v: Any) -> Any:
    tag, p = ext_tag(v, "Ty")
    if tag == "Int" and p is None:
        return ("Int",)
    if tag == "Float" and p is None:
        return ("Float",)
    if tag == "Bool" and p is None:
        return ("Bool",)
    if tag == "List":
        return ("List", decode_ty(p))
    if tag == "Set":
        return ("Set", decode_ty(p))
    if tag == "Map":
        arr = as_arr(p, "Map")
        if len(arr) != 2:
            raise DecodeError("Map expects [k,v]")
        return ("Map", decode_ty(arr[0]), decode_ty(arr[1]))
    if tag == "Option_":
        return ("Option", decode_ty(p))
    if tag == "Result_":
        arr = as_arr(p, "Result_")
        if len(arr) != 2:
            raise DecodeError("Result_ expects [t,e]")
        return ("Result", decode_ty(arr[0]), decode_ty(arr[1]))
    if tag == "Tuple":
        return ("Tuple", [decode_ty(x) for x in as_arr(p, "Tuple")])
    if tag == "Func":
        obj = expect_keys(p, ["params", "ret"], "Ty.Func")
        params = [decode_ty(x) for x in as_arr(obj["params"], "Ty.Func.params")]
        ret = None if obj["ret"] is None else decode_ty(obj["ret"])
        return ("Func", params, ret)
    if tag == "Record":
        return ("Record", as_str(p, "Ty.Record"))
    if tag == "Enum":
        return ("Enum", as_str(p, "Ty.Enum"))
    raise DecodeError(f"unknown Ty tag: {tag}")


def decode_maybe_ty(v: Any) -> Any:
    if v is None:
        return None
    return decode_ty(v)


def decode_place(v: Any) -> Place:
    tag, p = ext_tag(v, "Place")
    if tag == "Var":
        return Place("Var", name=as_str(p, "Place.Var"))
    if tag == "Index":
        obj = expect_keys(p, ["base", "base_ty", "index"], "Place.Index")
        return Place(
            "Index",
            base=decode_place(obj["base"]),
            base_ty=decode_ty(obj["base_ty"]),
            index=decode_expr(obj["index"]),
        )
    if tag == "Field":
        obj = expect_keys(p, ["base", "base_ty", "name"], "Place.Field")
        return Place(
            "Field",
            base=decode_place(obj["base"]),
            base_ty=decode_ty(obj["base_ty"]),
            field=as_str(obj["name"], "Place.Field.name"),
        )
    raise DecodeError(f"unknown Place: {tag}")


UNARY = {"Neg", "Not"}
BINARY = {"Add", "Sub", "Mul", "Div", "Mod", "Lt", "Le", "Gt", "Ge", "Eq", "Ne", "And", "Or"}
BUILTINS = {
    "AbsInt", "AbsFloat", "MinInt", "MaxInt", "MinFloat", "MaxFloat",
    "FloatOfInt", "IntOfFloat", "Floor", "Ceil", "Round", "Sqrt",
    "Filled", "NewMap", "NewSet",
    "ListLength", "ListAppend", "ListPop", "ListInsert", "ListRemoveAt", "ListSwap", "ListSort",
    "MapSize", "MapGet", "MapHas", "MapDelete", "MapKeys", "MapValues",
    "SetSize", "SetAdd", "SetHas", "SetRemove", "SetItems",
    "OptIsSome", "OptIsNone", "OptUnwrap", "OptGetOr",
    "ResIsOk", "ResIsErr", "ResUnwrap", "ResGetOr",
}


def decode_expr(v: Any) -> Expr:
    obj = expect_keys(v, ["ty", "kind"], "IrExpr")
    ty = decode_ty(obj["ty"])
    tag, p = ext_tag(obj["kind"], "IrExprKind")
    if tag == "Int":
        return Expr(ty, "Int", as_i64(p, "Int"))
    if tag == "Float":
        return Expr(ty, "Float", as_float(p, "Float"))
    if tag == "Bool":
        return Expr(ty, "Bool", as_bool(p, "Bool"))
    if tag == "Text":
        return Expr(ty, "Text", [as_i64_num(x, "Text") for x in as_arr(p, "Text")])
    if tag == "Local":
        return Expr(ty, "Local", as_str(p, "Local"))
    if tag == "Const":
        return Expr(ty, "Const", as_str(p, "Const"))
    if tag == "FuncRef":
        return Expr(ty, "FuncRef", as_str(p, "FuncRef"))
    if tag == "List":
        return Expr(ty, "List", [decode_expr(x) for x in as_arr(p, "List")])
    if tag == "Tuple":
        return Expr(ty, "Tuple", [decode_expr(x) for x in as_arr(p, "Tuple")])
    if tag == "CallFunc":
        o = expect_keys(p, ["name", "args"], "CallFunc")
        return Expr(ty, "CallFunc", (as_str(o["name"], "CallFunc.name"),
                                    [decode_expr(x) for x in as_arr(o["args"], "CallFunc.args")]))
    if tag == "CallValue":
        o = expect_keys(p, ["callee", "args"], "CallValue")
        return Expr(ty, "CallValue", (decode_expr(o["callee"]),
                                     [decode_expr(x) for x in as_arr(o["args"], "CallValue.args")]))
    if tag == "NewRecord":
        o = expect_keys(p, ["name", "args"], "NewRecord")
        return Expr(ty, "NewRecord", (as_str(o["name"], "NewRecord.name"),
                                     [decode_expr(x) for x in as_arr(o["args"], "NewRecord.args")]))
    if tag == "NewVariant":
        o = expect_keys(p, ["enum_name", "variant", "args"], "NewVariant")
        return Expr(ty, "NewVariant", (
            as_str(o["enum_name"], "NewVariant.enum_name"),
            as_str(o["variant"], "NewVariant.variant"),
            [decode_expr(x) for x in as_arr(o["args"], "NewVariant.args")],
        ))
    if tag == "Builtin":
        o = expect_keys(p, ["builtin", "args"], "Builtin")
        b = as_str(o["builtin"], "Builtin.builtin")
        if b not in BUILTINS:
            raise DecodeError(f"unknown Builtin: {b}")
        return Expr(ty, "Builtin", (b, [decode_expr(x) for x in as_arr(o["args"], "Builtin.args")]))
    if tag == "MutBuiltin":
        o = expect_keys(p, ["builtin", "recv", "recv_ty", "args"], "MutBuiltin")
        b = as_str(o["builtin"], "MutBuiltin.builtin")
        if b not in BUILTINS:
            raise DecodeError(f"unknown Builtin: {b}")
        return Expr(ty, "MutBuiltin", (
            b,
            decode_place(o["recv"]),
            decode_ty(o["recv_ty"]),
            [decode_expr(x) for x in as_arr(o["args"], "MutBuiltin.args")],
        ))
    if tag == "GetField":
        o = expect_keys(p, ["recv", "name"], "GetField")
        return Expr(ty, "GetField", (decode_expr(o["recv"]), as_str(o["name"], "GetField.name")))
    if tag == "Index":
        o = expect_keys(p, ["recv", "index"], "Index")
        return Expr(ty, "Index", (decode_expr(o["recv"]), decode_expr(o["index"])))
    if tag == "Unary":
        o = expect_keys(p, ["op", "operand"], "Unary")
        op = as_str(o["op"], "Unary.op")
        if op not in UNARY:
            raise DecodeError(f"unknown UnaryOp: {op}")
        return Expr(ty, "Unary", (op, decode_expr(o["operand"])))
    if tag == "Binary":
        o = expect_keys(p, ["op", "lhs", "rhs"], "Binary")
        op = as_str(o["op"], "Binary.op")
        if op not in BINARY:
            raise DecodeError(f"unknown BinaryOp: {op}")
        return Expr(ty, "Binary", (op, decode_expr(o["lhs"]), decode_expr(o["rhs"])))
    raise DecodeError(f"unknown IrExprKind: {tag}")


def decode_pattern(v: Any) -> Any:
    tag, p = ext_tag(v, "IrPattern")
    if tag == "Int":
        return ("Int", as_i64(p, "Pat.Int"))
    if tag == "Bool":
        return ("Bool", as_bool(p, "Pat.Bool"))
    if tag == "Wildcard" and p is None:
        return ("Wildcard",)
    if tag == "Variant":
        o = expect_keys(p, ["enum_name", "variant", "binders"], "Pat.Variant")
        return ("Variant", as_str(o["enum_name"], "variant.enum"),
                as_str(o["variant"], "variant.name"),
                [as_str(x, "binder") for x in as_arr(o["binders"], "binders")])
    raise DecodeError(f"unknown IrPattern: {tag}")


def decode_stmt(v: Any) -> Stmt:
    tag, p = ext_tag(v, "IrStmt")
    if tag == "Skip" and p is None:
        return Stmt("Skip")
    if tag == "Break" and p is None:
        return Stmt("Break")
    if tag == "Continue" and p is None:
        return Stmt("Continue")
    if tag == "Assign":
        o = expect_keys(p, ["target", "value", "declares"], "Assign")
        return Stmt("Assign", (decode_place(o["target"]), decode_expr(o["value"]),
                              as_bool(o["declares"], "Assign.declares")))
    if tag == "TupleAssign":
        o = expect_keys(p, ["targets", "declares", "value"], "TupleAssign")
        return Stmt("TupleAssign", (
            [as_str(x, "TupleAssign.targets") for x in as_arr(o["targets"], "targets")],
            [as_bool(x, "TupleAssign.declares") for x in as_arr(o["declares"], "declares")],
            decode_expr(o["value"]),
        ))
    if tag == "Expr":
        return Stmt("Expr", decode_expr(p))
    if tag == "If":
        o = expect_keys(p, ["arms", "else_block"], "If")
        arms = []
        for arm in as_arr(o["arms"], "If.arms"):
            arr = as_arr(arm, "If.arm")
            if len(arr) != 2:
                raise DecodeError("If arm must be [cond, body[]]")
            arms.append((decode_expr(arr[0]), [decode_stmt(s) for s in as_arr(arr[1], "If.body")]))
        else_b = None
        if o["else_block"] is not None:
            else_b = [decode_stmt(s) for s in as_arr(o["else_block"], "If.else")]
        return Stmt("If", (arms, else_b))
    if tag == "While":
        o = expect_keys(p, ["cond", "body"], "While")
        return Stmt("While", (decode_expr(o["cond"]), [decode_stmt(s) for s in as_arr(o["body"], "While.body")]))
    if tag == "ForRange":
        o = expect_keys(p, ["var", "from", "to", "down", "body"], "ForRange")
        return Stmt("ForRange", (
            as_str(o["var"], "ForRange.var"),
            decode_expr(o["from"]),
            decode_expr(o["to"]),
            as_bool(o["down"], "ForRange.down"),
            [decode_stmt(s) for s in as_arr(o["body"], "ForRange.body")],
        ))
    if tag == "ForIn":
        o = expect_keys(p, ["vars", "iter", "body"], "ForIn")
        return Stmt("ForIn", (
            [as_str(x, "ForIn.vars") for x in as_arr(o["vars"], "ForIn.vars")],
            decode_expr(o["iter"]),
            [decode_stmt(s) for s in as_arr(o["body"], "ForIn.body")],
        ))
    if tag == "Match":
        o = expect_keys(p, ["scrutinee", "arms"], "Match")
        arms = []
        for arm in as_arr(o["arms"], "Match.arms"):
            a = expect_keys(arm, ["pattern", "body"], "Match.arm")
            arms.append((decode_pattern(a["pattern"]),
                         [decode_stmt(s) for s in as_arr(a["body"], "Match.body")]))
        return Stmt("Match", (decode_expr(o["scrutinee"]), arms))
    if tag == "Return":
        if p is None:
            return Stmt("Return", None)
        return Stmt("Return", decode_expr(p))
    if tag == "Assert":
        o = expect_keys(p, ["cond", "line"], "Assert")
        return Stmt("Assert", (decode_expr(o["cond"]), as_line(o["line"], "Assert.line")))
    if tag == "ExpectTrap":
        o = expect_keys(p, ["kind", "body", "line"], "ExpectTrap")
        return Stmt("ExpectTrap", (
            as_str(o["kind"], "ExpectTrap.kind"),
            [decode_stmt(s) for s in as_arr(o["body"], "ExpectTrap.body")],
            as_line(o["line"], "ExpectTrap.line"),
        ))
    raise DecodeError(f"unknown IrStmt: {tag}")


def decode_field_pair(v: Any) -> tuple[str, Any]:
    o = expect_keys(v, ["name", "ty", "boundary"], "field")
    _ = o["boundary"]
    return as_str(o["name"], "field.name"), decode_ty(o["ty"])


def decode_param(v: Any) -> Param:
    o = expect_keys(v, ["name", "inout", "ty", "boundary", "never_written"], "IrParam")
    _ = o["boundary"]
    return Param(
        as_str(o["name"], "param.name"),
        as_bool(o["inout"], "param.inout"),
        decode_ty(o["ty"]),
        as_bool(o["never_written"], "param.never_written"),
    )


def decode_func(v: Any) -> Func:
    o = expect_keys(v, ["name", "export", "params", "ret", "ret_boundary", "body"], "IrFunc")
    _ = o["ret_boundary"]
    return Func(
        as_str(o["name"], "func.name"),
        as_bool(o["export"], "func.export"),
        [decode_param(x) for x in as_arr(o["params"], "func.params")],
        decode_maybe_ty(o["ret"]),
        [decode_stmt(s) for s in as_arr(o["body"], "func.body")],
    )


def decode_test(v: Any) -> Test:
    o = expect_keys(v, ["name", "body"], "IrTest")
    return Test(as_str(o["name"], "test.name"), [decode_stmt(s) for s in as_arr(o["body"], "test.body")])


def decode_record(v: Any) -> Record:
    o = expect_keys(v, ["name", "fields"], "IrRecord")
    return Record(as_str(o["name"], "record.name"),
                  [decode_field_pair(x) for x in as_arr(o["fields"], "record.fields")])


def decode_enum(v: Any) -> Enum:
    o = expect_keys(v, ["name", "variants"], "IrEnum")
    variants = []
    for item in as_arr(o["variants"], "enum.variants"):
        vo = expect_keys(item, ["name", "fields"], "variant")
        variants.append((
            as_str(vo["name"], "variant.name"),
            [decode_field_pair(x) for x in as_arr(vo["fields"], "variant.fields")],
        ))
    return Enum(as_str(o["name"], "enum.name"), variants)


def decode_const(v: Any) -> Const:
    o = expect_keys(v, ["name", "ty", "value"], "IrConst")
    return Const(as_str(o["name"], "const.name"), decode_ty(o["ty"]), decode_expr(o["value"]))


def decode_module(v: Any) -> Module:
    o = expect_keys(v, ["name", "imports", "records", "enums", "consts", "funcs", "tests"], "IrModule")
    return Module(
        as_str(o["name"], "module.name"),
        [as_str(x, "import") for x in as_arr(o["imports"], "imports")],
        [decode_record(x) for x in as_arr(o["records"], "records")],
        [decode_enum(x) for x in as_arr(o["enums"], "enums")],
        [decode_const(x) for x in as_arr(o["consts"], "consts")],
        [decode_func(x) for x in as_arr(o["funcs"], "funcs")],
        [decode_test(x) for x in as_arr(o["tests"], "tests")],
    )


def decode_request(v: Any) -> EmitReq:
    o = expect_keys(v, ["protocol", "cmd", "entry", "with_tests", "modules"], "emit request")
    proto = o["protocol"]
    if proto != 4:
        raise DecodeError(
            f"PROTOCOL MISMATCH: request stamped protocol {proto!r} but this "
            "emitter speaks protocol 4 (mismatched sudoc/backend toolchain pair)"
        )
    cmd = as_str(o["cmd"], "cmd")
    if cmd != "emit":
        raise DecodeError(f"unknown cmd: {cmd}")
    entry = as_str(o["entry"], "entry")
    with_tests = as_bool(o["with_tests"], "with_tests")
    mods = [decode_module(x) for x in as_arr(o["modules"], "modules")]
    if not mods:
        raise DecodeError("modules must be non-empty")
    if mods[-1].name != entry:
        raise DecodeError(f"entry {entry!r} != last module {mods[-1].name!r}")
    return EmitReq(entry, with_tests, mods)


# ---------------------------------------------------------------------------
# Naming
# ---------------------------------------------------------------------------

LEAN_RESERVED = {
    "abbrev", "axiom", "background", "by", "calc", "catch", "class", "coe",
    "command", "conv", "def", "deriving", "do", "elab", "else", "end",
    "example", "exists", "export", "extends", "finally", "for", "forall",
    "fun", "have", "hiding", "if", "import", "in", "include", "inductive",
    "infix", "infixl", "infixr", "instance", "lemma", "let", "local",
    "macro", "match", "mutual", "namespace", "noncomputable", "notation",
    "obtain", "opaque", "open", "postfix", "precedence", "prefix", "private",
    "protected", "rec", "renaming", "return", "section", "set_option", "skip",
    "sorry", "structure", "syntax", "then", "theorem", "try", "universe",
    "unless", "unsafe", "using", "variable", "where", "with", "nomatch",
    "break", "continue", "partial", "scoped", "initialize", "meta",
    "Type", "Prop", "Sort", "Unit", "Bool", "Nat", "Int", "Float", "String",
    "Char", "List", "Array", "Option", "Except", "IO", "Id", "True", "False",
    "Empty", "PUnit", "UInt32", "UInt64", "Int64", "Ordering", "none", "some",
    "pure", "discard", "main", "SudoRt", "SudoM", "Trap", "Step",
}


def mangle_value(n: str) -> str:
    base = n
    if base in LEAN_RESERVED or base.lower() in {x.lower() for x in LEAN_RESERVED}:
        base = "sudo_" + base
    if base[:1].isdigit():
        base = "v_" + base
    return base


def mangle_type(n: str) -> str:
    base = n
    if base and base[0].islower():
        base = base[0].upper() + base[1:]
    if base in LEAN_RESERVED or base.lower() in {x.lower() for x in LEAN_RESERVED}:
        base = "T_" + base
    return base


def mangle_module(n: str) -> str:
    return mangle_value(n)


def enc_len(s: str) -> str:
    return f"{len(s)}{s}"


def mangle_field(rec: str, field: str) -> str:
    return f"sudo_{enc_len(mangle_type(rec))}_{enc_len(field)}"


def mangle_variant(en: str, vn: str) -> str:
    return f"Sudo_{enc_len(mangle_type(en))}_{enc_len(mangle_type(vn))}"


def split_qual(name: str) -> tuple[Optional[str], str]:
    if "." in name:
        a, b = name.split(".", 1)
        if a and b:
            return a, b
    return None, name


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
    if not out:
        return "t"
    return "".join(out)


def test_fn_names(tests: list[Test]) -> list[str]:
    used: set[str] = set()
    names: list[str] = []
    for t in tests:
        name = f"test_{sanitize_test(t.name)}"
        n = 2
        while name in used:
            name = f"test_{sanitize_test(t.name)}_{n}"
            n += 1
        used.add(name)
        names.append(name)
    return names


# ---------------------------------------------------------------------------
# Refuse non-total constructs
# ---------------------------------------------------------------------------

def walk_stmts(stmts: list[Stmt], fn: Callable[[Stmt], None]) -> None:
    for s in stmts:
        fn(s)
        p = s.payload
        if s.kind == "If":
            for _, body in p[0]:
                walk_stmts(body, fn)
            if p[1]:
                walk_stmts(p[1], fn)
        elif s.kind == "While":
            walk_stmts(p[1], fn)
        elif s.kind == "ForRange":
            walk_stmts(p[4], fn)
        elif s.kind == "ForIn":
            walk_stmts(p[2], fn)
        elif s.kind == "Match":
            for _, body in p[1]:
                walk_stmts(body, fn)
        elif s.kind == "ExpectTrap":
            walk_stmts(p[1], fn)


def has_while(stmts: list[Stmt]) -> bool:
    found = False

    def vis(s: Stmt) -> None:
        nonlocal found
        if s.kind == "While":
            found = True

    walk_stmts(stmts, vis)
    return found


def call_names_in_expr(e: Expr, out: list[str]) -> None:
    k, p = e.kind, e.payload
    if k == "CallFunc":
        out.append(p[0])
        for a in p[1]:
            call_names_in_expr(a, out)
    elif k in ("List", "Tuple"):
        for a in p:
            call_names_in_expr(a, out)
    elif k in ("CallValue",):
        call_names_in_expr(p[0], out)
        for a in p[1]:
            call_names_in_expr(a, out)
    elif k in ("NewRecord",):
        for a in p[1]:
            call_names_in_expr(a, out)
    elif k == "NewVariant":
        for a in p[2]:
            call_names_in_expr(a, out)
    elif k == "Builtin":
        for a in p[1]:
            call_names_in_expr(a, out)
    elif k == "MutBuiltin":
        walk_place_expr(p[1], out)
        for a in p[3]:
            call_names_in_expr(a, out)
    elif k in ("GetField", "Index"):
        call_names_in_expr(p[0], out)
        if k == "Index":
            call_names_in_expr(p[1], out)
    elif k == "Unary":
        call_names_in_expr(p[1], out)
    elif k == "Binary":
        call_names_in_expr(p[1], out)
        call_names_in_expr(p[2], out)


def walk_place_expr(pl: Place, out: list[str]) -> None:
    if pl.kind == "Index":
        walk_place_expr(pl.base, out)
        call_names_in_expr(pl.index, out)
    elif pl.kind == "Field":
        walk_place_expr(pl.base, out)


def calls_in_stmts(stmts: list[Stmt]) -> list[str]:
    out: list[str] = []

    def vis(s: Stmt) -> None:
        p = s.payload
        if s.kind == "Assign":
            walk_place_expr(p[0], out)
            call_names_in_expr(p[1], out)
        elif s.kind == "TupleAssign":
            call_names_in_expr(p[2], out)
        elif s.kind == "Expr":
            call_names_in_expr(p, out)
        elif s.kind == "If":
            for c, _ in p[0]:
                call_names_in_expr(c, out)
        elif s.kind == "While":
            call_names_in_expr(p[0], out)
        elif s.kind == "ForRange":
            call_names_in_expr(p[1], out)
            call_names_in_expr(p[2], out)
        elif s.kind == "ForIn":
            call_names_in_expr(p[1], out)
        elif s.kind == "Match":
            call_names_in_expr(p[0], out)
        elif s.kind == "Return" and p is not None:
            call_names_in_expr(p, out)
        elif s.kind == "Assert":
            call_names_in_expr(p[0], out)

    walk_stmts(stmts, vis)
    return out


def func_id(module: str, call: str) -> tuple[str, str]:
    mq, fn = split_qual(call)
    return (mq or module, fn)


def refuse_nontotal(req: EmitReq) -> Optional[str]:
    reasons: list[str] = []
    for m in req.modules:
        for f in m.funcs:
            if has_while(f.body):
                reasons.append(
                    f"{m.name}.{f.name}: while loops are not emitted as Lean total "
                    "defs (decreases is erased from the IR). Use for-range/for-in, "
                    "or wait for measure-carrying IR."
                )
        for t in m.tests:
            if has_while(t.body):
                reasons.append(
                    f'{m.name} test "{t.name}": while loops are not emitted as Lean '
                    "total defs (decreases is erased from the IR)."
                )

    # Recursion / mutual recursion among functions (tests may call them).
    ids = [(m.name, f.name) for m in req.modules for f in m.funcs]
    graph: dict[tuple[str, str], set[tuple[str, str]]] = {i: set() for i in ids}
    bodies = {(m.name, f.name): f.body for m in req.modules for f in m.funcs}
    for (mod, name), body in bodies.items():
        for c in calls_in_stmts(body):
            cal = func_id(mod, c)
            if cal in graph:
                graph[(mod, name)].add(cal)

    # Tarjan-ish cycle detection via reachability
    def reaches_self(start: tuple[str, str]) -> bool:
        seen: set[tuple[str, str]] = set()
        stack = list(graph[start])
        while stack:
            n = stack.pop()
            if n == start:
                return True
            if n in seen:
                continue
            seen.add(n)
            stack.extend(graph.get(n, ()))
        return False

    for ident in ids:
        if reaches_self(ident):
            reasons.append(
                f"{ident[0]}.{ident[1]}: recursive functions are refused — "
                "Lean cannot reconstruct a termination measure from protocol-4 IR."
            )

    if reasons:
        return "lean backend refuses non-total constructs:\n  - " + "\n  - ".join(reasons)
    return None


# ---------------------------------------------------------------------------
# Written-local analysis (loop state)
# ---------------------------------------------------------------------------

def place_root(pl: Place) -> Optional[str]:
    while pl is not None:
        if pl.kind == "Var":
            return pl.name
        pl = pl.base
    return None


def written_in_stmts(stmts: list[Stmt]) -> set[str]:
    out: set[str] = set()

    def vis(s: Stmt) -> None:
        p = s.payload
        if s.kind == "Assign":
            r = place_root(p[0])
            if r:
                out.add(r)
        elif s.kind == "TupleAssign":
            out.update(p[0])
        elif s.kind == "Expr" and p.kind == "MutBuiltin":
            r = place_root(p.payload[1])
            if r:
                out.add(r)
        elif s.kind == "Expr" and p.kind == "CallFunc":
            pass  # inout writeback handled at emit time via lookup

    walk_stmts(stmts, vis)
    return out


# ---------------------------------------------------------------------------
# Emitter
# ---------------------------------------------------------------------------

@dataclass
class Ctx:
    module: Module
    all_mods: list[Module]
    fresh: int = 0
    inouts: list[str] = field(default_factory=list)
    locals: list[tuple[str, Any]] = field(default_factory=list)
    loop_vars: list[str] = field(default_factory=list)
    ret_ty: Any = None
    in_loop: bool = False
    observe: bool = False
    caught: str = ""

    def gensym(self, prefix: str = "t") -> str:
        self.fresh += 1
        return f"__{prefix}{self.fresh}"


def type_home(all_mods: list[Module], name: str) -> Optional[str]:
    for m in all_mods:
        if any(r.name == name for r in m.records) or any(e.name == name for e in m.enums):
            return m.name
    return None


def lookup_func(ctx: Ctx, name: str) -> Optional[Func]:
    mq, fn = split_qual(name)
    if mq is None:
        for f in ctx.module.funcs:
            if f.name == fn:
                return f
        return None
    for m in ctx.all_mods:
        if m.name == mq:
            for f in m.funcs:
                if f.name == fn:
                    return f
    return None


def qual_nominal(ctx: Ctx, name: str, local: str) -> str:
    home = type_home(ctx.all_mods, name)
    if home is None:
        raise DecodeError(f"internal error: nominal type {name!r} has no declaring module")
    if home != ctx.module.name:
        return f"{mangle_module(home)}.{local}"
    return local


def render_ty(ctx: Ctx, ty: Any) -> str:
    tag = ty[0]
    if tag == "Int":
        return "Int"
    if tag == "Float":
        return "Float"
    if tag == "Bool":
        return "Bool"
    if tag == "List":
        return f"(Array {render_ty(ctx, ty[1])})"
    if tag == "Set":
        return f"(SudoRt.SSet {render_ty(ctx, ty[1])})"
    if tag == "Map":
        return f"(SudoRt.SMap {render_ty(ctx, ty[1])} {render_ty(ctx, ty[2])})"
    if tag == "Option":
        return f"(Option {render_ty(ctx, ty[1])})"
    if tag == "Result":
        return f"(Except {render_ty(ctx, ty[2])} {render_ty(ctx, ty[1])})"
    if tag == "Tuple":
        ts = ty[1]
        if not ts:
            return "Unit"
        if len(ts) == 1:
            return render_ty(ctx, ts[0])
        return "(" + " × ".join(render_ty(ctx, t) for t in ts) + ")"
    if tag == "Func":
        params, ret = ty[1], ty[2]
        args = [render_ty(ctx, p) for p in params]
        r = "Unit" if ret is None else render_ty(ctx, ret)
        return "(" + " → ".join(args + [r]) + ")"
    if tag == "Record":
        return qual_nominal(ctx, ty[1], mangle_type(ty[1]))
    if tag == "Enum":
        return qual_nominal(ctx, ty[1], mangle_type(ty[1]))
    raise DecodeError(f"unhandled ty {ty}")


def fret_parts(f: Func) -> list[Any]:
    parts = []
    if f.ret is not None:
        parts.append(f.ret)
    parts.extend(p.ty for p in f.params if p.inout)
    return parts


def fret_ty_str(ctx: Ctx, f: Func) -> str:
    parts = fret_parts(f)
    if not parts:
        return "Unit"
    if len(parts) == 1:
        return render_ty(ctx, parts[0])
    return "(" + " × ".join(render_ty(ctx, t) for t in parts) + ")"


def render_int(n: int) -> str:
    return f"({n} : Int)"


def render_float(x: float) -> str:
    if math.isnan(x):
        return "(0.0 / 0.0)"
    if math.isinf(x):
        return "(1.0 / 0.0)" if x > 0 else "(-(1.0 / 0.0))"
    # Signed zero must stay negative: Lean `0.0` is +0.0.
    if x == 0.0 and math.copysign(1.0, x) < 0.0:
        return "(-0.0)"
    # shortest round-trip
    s = repr(float(x))
    if s.endswith(".0"):
        pass
    elif "." not in s and "e" not in s and "E" not in s:
        s = s + ".0"
    # Parenthesize negatives so `fround -2.5` is not subtraction.
    if s.startswith("-"):
        return f"({s})"
    return s


def result_ty_of_func(f: Optional[Func], fallback: Any) -> Any:
    if f is None:
        return fallback
    parts = fret_parts(f)
    if not parts:
        return None
    if len(parts) == 1:
        return parts[0]
    return ("Tuple", parts)


def qual_value(ctx: Ctx, name: str) -> str:
    mq, fn = split_qual(name)
    local = mangle_value(fn)
    if mq is None or mq == ctx.module.name:
        return local
    return f"{mangle_module(mq)}.{local}"


def emit_const_ref(ctx: Ctx, name: str) -> str:
    return qual_value(ctx, name)


class Emitter:
    def __init__(self, ctx: Ctx) -> None:
        self.ctx = ctx
        self.lines: list[str] = []

    def add(self, s: str) -> None:
        self.lines.append(s)

    def bind(self, expr_code: str, is_pure: bool, ty: Any = None) -> str:
        if is_pure:
            return expr_code
        tmp = self.ctx.gensym()
        if self.ctx.observe:
            dflt = default_value(self.ctx, ty)
            self.add(f"let mut {tmp} := {dflt}")
            self.add(f"if {self.ctx.caught}.isNone then")
            self.add(f"  match EStateM.run ({expr_code}) () with")
            self.add(f"  | .ok __ok _ => {tmp} := __ok")
            self.add(f"  | .error __err _ => {self.ctx.caught} := some __err")
            return tmp
        self.add(f"let {tmp} ← {expr_code}")
        return tmp

    def bind_e(self, e: Expr) -> str:
        c, p = self.emit_expr(e)
        return c if p else self.bind(c, False, e.ty)

    def bind_m(self, expr_code: str, ty: Any) -> str:
        return self.bind(expr_code, False, ty)

    def bind_step(self, expr_code: str, default_cont: str) -> str:
        """Bind a `SudoRt.Step` action; default is `.cont` of the current state."""
        tmp = self.ctx.gensym("st")
        if self.ctx.observe:
            self.add(f"let mut {tmp} := SudoRt.Step.cont {default_cont}")
            self.add(f"if {self.ctx.caught}.isNone then")
            self.add(f"  match EStateM.run ({expr_code}) () with")
            self.add(f"  | .ok __ok _ => {tmp} := __ok")
            self.add(f"  | .error __err _ => {self.ctx.caught} := some __err")
            return tmp
        self.add(f"let {tmp} ← {expr_code}")
        return tmp

    def emit_expr(self, e: Expr) -> tuple[str, bool]:
        """Return (lean_expr, is_pure). Side-effecting lets are written first."""
        k, p = e.kind, e.payload
        if k == "Int":
            return render_int(p), True
        if k == "Float":
            return render_float(p), True
        if k == "Bool":
            return ("true" if p else "false"), True
        if k == "Text":
            elems = ", ".join(render_int(n) for n in p)
            return f"(#[{elems}] : Array Int)", True
        if k == "Local":
            return mangle_value(p), True
        if k == "Const":
            return emit_const_ref(self.ctx, p), True
        if k == "FuncRef":
            return qual_value(self.ctx, p), True
        if k == "List":
            parts = [self.bind_e(a) for a in p]
            inner = ", ".join(parts)
            return f"(#[{inner}] : {render_ty(self.ctx, e.ty)})", True
        if k == "Tuple":
            parts = [self.bind_e(a) for a in p]
            if not parts:
                return "()", True
            if len(parts) == 1:
                return parts[0], True
            return "(" + ", ".join(parts) + ")", True
        if k == "CallFunc":
            return self.emit_call(p[0], p[1], e.ty)
        if k == "CallValue":
            cal, args = p
            cval = self.bind_e(cal)
            avs = [self.bind_e(a) for a in args]
            call = " ".join([cval] + avs) if avs else cval
            return self.bind_m(call, e.ty), True
        if k == "NewRecord":
            name, args = p
            avs = []
            rec = None
            for m in self.ctx.all_mods:
                for r in m.records:
                    if r.name == name:
                        rec = r
            avs = [self.bind_e(a) for a in args]
            tn = qual_nominal(self.ctx, name, mangle_type(name))
            if rec is None:
                fields = [f"f{i}" for i in range(len(avs))]
            else:
                fields = [mangle_field(name, fn) for fn, _ in rec.fields]
            assigns = ", ".join(f"{f} := {v}" for f, v in zip(fields, avs))
            return f"{{ {assigns} : {tn} }}", True
        if k == "NewVariant":
            en, vn, args = p
            avs = [self.bind_e(a) for a in args]
            if en == "Option" and vn == "Some":
                return f"(some {avs[0]})", True
            if en == "Option" and vn == "None":
                return "none", True
            if en == "Result" and vn == "Ok":
                return f"(Except.ok {avs[0]})", True
            if en == "Result" and vn == "Err":
                return f"(Except.error {avs[0]})", True
            cn = qual_nominal(self.ctx, en, mangle_variant(en, vn))
            if avs:
                return f"({cn} {' '.join(avs)})", True
            return cn, True
        if k == "Builtin":
            return self.emit_builtin(p[0], p[1], e.ty)
        if k == "MutBuiltin":
            return self.emit_mut_builtin(p[0], p[1], p[2], p[3], e.ty)
        if k == "GetField":
            recv, fname = p
            rval = self.bind_e(recv)
            rec_name = recv.ty[1] if recv.ty[0] == "Record" else ""
            return f"{rval}.{mangle_field(rec_name, fname)}", True
        if k == "Index":
            recv, idx = p
            rval = self.bind_e(recv)
            ival = self.bind_e(idx)
            if recv.ty[0] == "List":
                return self.bind_m(f"SudoRt.listGet {rval} {ival}", e.ty), True
            if recv.ty[0] == "Map":
                return self.bind_m(f"SudoRt.mapGet {rval} {ival}", e.ty), True
            raise DecodeError(f"index on {recv.ty}")
        if k == "Unary":
            op, operand = p
            oval = self.bind_e(operand)
            if op == "Not":
                return f"(!{oval})", True
            # Float negation is total (incl. signed zero). Int negation is i64-checked.
            if e.ty[0] == "Float" or operand.ty[0] == "Float":
                return f"(-{oval})", True
            return self.bind_m(f"SudoRt.negI {oval}", ("Int",)), True
        if k == "Binary":
            return self.emit_binary(p[0], p[1], p[2], e.ty)
        raise DecodeError(f"unhandled expr {k}")

    def emit_binary(self, op: str, lhs: Expr, rhs: Expr, ty: Any) -> tuple[str, bool]:
        if op == "And":
            lval = self.bind_e(lhs)
            inner = Emitter(self.ctx)
            rval = inner.bind_e(rhs)
            if not inner.lines:
                return self.bind_m(f"(if {lval} then pure {rval} else pure false)", ("Bool",)), True
            rhs_do = self._do_block(inner.lines, f"pure {rval}")
            return self.bind_m(f"(if {lval} then {rhs_do} else pure false)", ("Bool",)), True
        if op == "Or":
            lval = self.bind_e(lhs)
            inner = Emitter(self.ctx)
            rval = inner.bind_e(rhs)
            if not inner.lines:
                return self.bind_m(f"(if {lval} then pure true else pure {rval})", ("Bool",)), True
            rhs_do = self._do_block(inner.lines, f"pure {rval}")
            return self.bind_m(f"(if {lval} then pure true else {rhs_do})", ("Bool",)), True

        lval = self.bind_e(lhs)
        rval = self.bind_e(rhs)
        if op == "Add" and ty[0] == "List":
            return f"(SudoRt.listConcat {lval} {rval})", True
        arith = {
            "Add": "SudoRt.addI", "Sub": "SudoRt.subI", "Mul": "SudoRt.mulI",
            "Div": "SudoRt.divI", "Mod": "SudoRt.modI",
        }
        if op in arith and lhs.ty[0] == "Int":
            return self.bind_m(f"{arith[op]} {lval} {rval}", ("Int",)), True
        if op in ("Add", "Sub", "Mul", "Div") and lhs.ty[0] == "Float":
            if op == "Add":
                return f"({lval} + {rval})", True
            if op == "Sub":
                return f"({lval} - {rval})", True
            if op == "Mul":
                return f"({lval} * {rval})", True
            return f"(SudoRt.fdiv {lval} {rval})", True
        cmp = {"Lt": "<", "Le": "≤", "Gt": ">", "Ge": "≥"}
        if op in cmp and lhs.ty[0] in ("Int", "Float"):
            return f"({lval} {cmp[op]} {rval})", True
        if op == "Eq":
            return f"(SudoRt.eq {lval} {rval})", True
        if op == "Ne":
            return f"(!(SudoRt.eq {lval} {rval}))", True
        raise DecodeError(f"unhandled binary {op} on {lhs.ty}")

    def _do_block(self, lines: list[str], last: str) -> str:
        body = [ln for ln in lines if ln]
        body.append(last)
        if len(body) == 1 and "\n" not in body[0]:
            return body[0]
        return "(do\n  " + "\n  ".join(body) + ")"

    def emit_call(self, name: str, args: list[Expr], ty: Any) -> tuple[str, bool]:
        avs = [self.bind_e(a) for a in args]
        fn = qual_value(self.ctx, name)
        call = " ".join([fn] + avs) if avs else fn
        callee = lookup_func(self.ctx, name)
        tmp = self.bind_m(call, result_ty_of_func(callee, ty))
        if callee:
            inouts = [p.name for p in callee.params if p.inout]
            if inouts:
                # unpack (ret?, *inouts) and write back
                if callee.ret is None:
                    names = [self.ctx.gensym("io") for _ in inouts]
                    if len(names) == 1:
                        self.add(f"let {names[0]} := {tmp}")
                    else:
                        self.add(f"let ({', '.join(names)}) := {tmp}")
                    for nm, orig in zip(names, inouts):
                        self.add(f"{mangle_value(orig)} := {nm}")
                    return "()", True
                retn = self.ctx.gensym("r")
                names = [self.ctx.gensym("io") for _ in inouts]
                self.add(f"let ({', '.join([retn] + names)}) := {tmp}")
                for nm, orig in zip(names, inouts):
                    self.add(f"{mangle_value(orig)} := {nm}")
                return retn, True
        return tmp, True

    def emit_builtin(self, b: str, args: list[Expr], ty: Any) -> tuple[str, bool]:
        avs = [self.bind_e(a) for a in args]

        def u(*xs: str) -> tuple[str, bool]:
            return " ".join(xs), True

        def m(code: str) -> tuple[str, bool]:
            return self.bind_m(code, ty), True

        if b == "AbsInt":
            return m(f"SudoRt.absI {avs[0]}")
        if b == "AbsFloat":
            return u(f"(SudoRt.fabs {avs[0]})")
        if b == "MinInt":
            return u(f"(SudoRt.minI {avs[0]} {avs[1]})")
        if b == "MaxInt":
            return u(f"(SudoRt.maxI {avs[0]} {avs[1]})")
        if b == "MinFloat":
            return u(f"(SudoRt.fmin {avs[0]} {avs[1]})")
        if b == "MaxFloat":
            return u(f"(SudoRt.fmax {avs[0]} {avs[1]})")
        if b == "FloatOfInt":
            return u(f"(SudoRt.floatOfInt {avs[0]})")
        if b == "IntOfFloat":
            return m(f"SudoRt.intOfFloat {avs[0]}")
        if b == "Floor":
            return u(f"(SudoRt.ffloor {avs[0]})")
        if b == "Ceil":
            return u(f"(SudoRt.fceil {avs[0]})")
        if b == "Round":
            return u(f"(SudoRt.fround {avs[0]})")
        if b == "Sqrt":
            return u(f"(SudoRt.fsqrt {avs[0]})")
        if b == "Filled":
            return m(f"SudoRt.filled {avs[0]} {avs[1]}")
        if b == "NewMap":
            return u("({} : " + render_ty(self.ctx, ty) + ")") if False else u("{ pairs := #[] }")
        if b == "NewSet":
            return u("{ items := #[] }")
        if b == "ListLength":
            return u(f"(Int.ofNat ({avs[0]}).size)")
        if b == "MapSize":
            return u(f"(SudoRt.mapSize {avs[0]})")
        if b == "MapGet":
            return m(f"SudoRt.mapGet {avs[0]} {avs[1]}")
        if b == "MapHas":
            return u(f"(SudoRt.mapHas {avs[0]} {avs[1]})")
        if b == "MapKeys":
            return u(f"(SudoRt.mapKeys {avs[0]})")
        if b == "MapValues":
            return u(f"(SudoRt.mapValues {avs[0]})")
        if b == "SetSize":
            return u(f"(SudoRt.setSize {avs[0]})")
        if b == "SetHas":
            return u(f"(SudoRt.setHas {avs[0]} {avs[1]})")
        if b == "SetItems":
            return u(f"(SudoRt.setItems {avs[0]})")
        if b == "OptIsSome":
            return u(f"({avs[0]}).isSome")
        if b == "OptIsNone":
            return u(f"({avs[0]}).isNone")
        if b == "OptUnwrap":
            return m(f"SudoRt.optUnwrap {avs[0]}")
        if b == "OptGetOr":
            return u(f"(SudoRt.optGetOr {avs[0]} {avs[1]})")
        if b == "ResIsOk":
            return u(f"(match {avs[0]} with | .ok _ => true | .error _ => false)")
        if b == "ResIsErr":
            return u(f"(match {avs[0]} with | .ok _ => false | .error _ => true)")
        if b == "ResUnwrap":
            return m(f"SudoRt.resUnwrap {avs[0]}")
        if b == "ResGetOr":
            return u(f"(SudoRt.resGetOr {avs[0]} {avs[1]})")
        raise DecodeError(f"builtin {b} is mutating or unhandled at expr site")

    def emit_mut_builtin(self, b: str, recv: Place, recv_ty: Any, args: list[Expr], ty: Any) -> tuple[str, bool]:
        # Evaluate place indices first (spec §12), then args, then mutate.
        root, get_code, set_fn = self.place_access(recv)
        avs = [self.bind_e(a) for a in args]
        cur = self.ctx.gensym("mv")
        self.add(f"let {cur} := {get_code}")
        if b == "ListAppend":
            self.add(f"{set_fn(f'(SudoRt.listAppend {cur} {avs[0]})')}")
            return "()", True
        if b == "ListPop":
            elem = recv_ty[1]
            tmp = self.bind_m(f"SudoRt.listPop {cur}", ("Tuple", [elem, recv_ty]))
            val, new = self.ctx.gensym("pv"), self.ctx.gensym("pl")
            self.add(f"let ({val}, {new}) := {tmp}")
            self.add(set_fn(new))
            return val, True
        if b == "ListInsert":
            tmp = self.bind_m(f"SudoRt.listInsert {cur} {avs[0]} {avs[1]}", recv_ty)
            self.add(set_fn(tmp))
            return "()", True
        if b == "ListRemoveAt":
            elem = recv_ty[1]
            tmp = self.bind_m(f"SudoRt.listRemoveAt {cur} {avs[0]}", ("Tuple", [elem, recv_ty]))
            val, new = self.ctx.gensym("rv"), self.ctx.gensym("rl")
            self.add(f"let ({val}, {new}) := {tmp}")
            self.add(set_fn(new))
            return val, True
        if b == "ListSwap":
            tmp = self.bind_m(f"SudoRt.listSwap {cur} {avs[0]} {avs[1]}", recv_ty)
            self.add(set_fn(tmp))
            return "()", True
        if b == "ListSort":
            if recv_ty[0] == "List" and recv_ty[1][0] == "Float":
                self.add(set_fn(f"(SudoRt.sortFloats {cur})"))
            else:
                self.add(set_fn(f"(SudoRt.sortInts {cur})"))
            return "()", True
        if b == "MapDelete":
            tmp = self.ctx.gensym("md")
            self.add(f"let {tmp} := SudoRt.mapDelete {cur} {avs[0]}")
            val, new = self.ctx.gensym("mdv"), self.ctx.gensym("mdm")
            self.add(f"let ({val}, {new}) := {tmp}")
            self.add(set_fn(new))
            return val, True
        if b == "SetAdd":
            tmp = self.ctx.gensym("sa")
            self.add(f"let {tmp} := SudoRt.setAdd {cur} {avs[0]}")
            val, new = self.ctx.gensym("sav"), self.ctx.gensym("sas")
            self.add(f"let ({val}, {new}) := {tmp}")
            self.add(set_fn(new))
            return val, True
        if b == "SetRemove":
            tmp = self.ctx.gensym("sr")
            self.add(f"let {tmp} := SudoRt.setRemove {cur} {avs[0]}")
            val, new = self.ctx.gensym("srv"), self.ctx.gensym("srs")
            self.add(f"let ({val}, {new}) := {tmp}")
            self.add(set_fn(new))
            return val, True
        raise DecodeError(f"unhandled MutBuiltin {b}")

    def place_access(self, pl: Place, for_read: bool = True) -> tuple[str, str, Callable[[str], str]]:
        """Returns (root_name, get_code, set_code_fn). Indices evaluated here."""
        if pl.kind == "Var":
            nm = mangle_value(pl.name)
            return pl.name, nm, lambda v: f"{nm} := {v}"
        if pl.kind == "Index":
            root, get_b, set_b = self.place_access(pl.base, for_read=True)
            ival = self.bind_e(pl.index)
            tmp = self.ctx.gensym("pg")
            if pl.base_ty[0] == "List":
                elem_ty = pl.base_ty[1]
                if for_read:
                    tmp = self.bind_m(f"SudoRt.listGet {get_b} {ival}", elem_ty)
                def setter(v: str, get_b=get_b, ival=ival, set_b=set_b, lty=pl.base_ty) -> str:
                    n = self.bind_m(f"SudoRt.listSet {get_b} {ival} {v}", lty)
                    return set_b(n)
                return root, tmp if for_read else get_b, setter
            if pl.base_ty[0] == "Map":
                val_ty = pl.base_ty[2]
                if for_read:
                    tmp = self.bind_m(f"SudoRt.mapGet {get_b} {ival}", val_ty)
                def setter(v: str, get_b=get_b, ival=ival, set_b=set_b) -> str:
                    return set_b(f"(SudoRt.mapInsert {get_b} {ival} {v})")
                return root, tmp if for_read else get_b, setter
            raise DecodeError(f"index place on {pl.base_ty}")
        if pl.kind == "Field":
            root, get_b, set_b = self.place_access(pl.base, for_read=True)
            rec_name = pl.base_ty[1] if pl.base_ty[0] == "Record" else ""
            fld = mangle_field(rec_name, pl.field)
            get = f"{get_b}.{fld}"
            def setter(v: str, get_b=get_b, fld=fld, set_b=set_b) -> str:
                return set_b(f"{{ {get_b} with {fld} := {v} }}")
            return root, get, setter
        raise DecodeError(f"unknown place {pl.kind}")

    def pack_state(self, names: list[str]) -> str:
        vs = [mangle_value(n) for n in names]
        if not vs:
            return "()"
        if len(vs) == 1:
            return vs[0]
        return "(" + ", ".join(vs) + ")"

    def unpack_state(self, names: list[str], src: str, *, let_mut: bool = False) -> None:
        vs = [mangle_value(n) for n in names]
        if not vs:
            return
        prefix = "let mut " if let_mut else ""
        assign = " := " if let_mut else " := "
        if let_mut:
            if len(vs) == 1:
                self.add(f"let mut {vs[0]} := {src}")
                return
            tmps = [self.ctx.gensym("u") for _ in vs]
            self.add(f"let ({', '.join(tmps)}) := {src}")
            for v, t in zip(vs, tmps):
                self.add(f"let mut {v} := {t}")
            return
        if len(vs) == 1:
            self.add(f"{vs[0]} := {src}")
            return
        tmps = [self.ctx.gensym("u") for _ in vs]
        self.add(f"let ({', '.join(tmps)}) := {src}")
        for v, t in zip(vs, tmps):
            self.add(f"{v} := {t}")

    def ret_value(self) -> str:
        parts = []
        # function return is assembled by caller via Stmt.Return
        return ""

    def emit_return(self, val: Optional[Expr]) -> None:
        pieces: list[str] = []
        if val is not None:
            pieces.append(self.bind_e(val))
        for name in self.ctx.inouts:
            pieces.append(mangle_value(name))
        if not pieces:
            code = "()"
        elif len(pieces) == 1:
            code = pieces[0]
        else:
            code = "(" + ", ".join(pieces) + ")"
        if self.ctx.in_loop:
            self.add(f"pure (.ret {code})")
        else:
            self.add(f"return {code}")

    def emit_stmts(self, stmts: list[Stmt], terminal: Optional[str] = None) -> None:
        for s in stmts:
            self.emit_stmt(s)
        if terminal:
            self.add(terminal)

    def emit_stmt(self, s: Stmt) -> None:
        if self.ctx.observe and self.ctx.caught and s.kind not in ("Skip", "ExpectTrap"):
            # Skip leftover statements after a trap was observed. `then do`
            # shares `let mut` with the enclosing do (Lean 4.14).
            self.add(f"if {self.ctx.caught}.isNone then do")
            inner = Emitter(self.ctx)
            inner._emit_stmt_body(s)
            if not inner.lines:
                inner.add("pure ()")
            for ln in inner.lines:
                self.add("  " + ln)
            return
        self._emit_stmt_body(s)

    def _emit_stmt_body(self, s: Stmt) -> None:
        k, p = s.kind, s.payload
        if k == "Skip":
            self.add("pure ()")
            return
        if k == "Break":
            if not self.ctx.in_loop:
                raise DecodeError("break outside loop")
            self.add(f"pure (.brk {self.pack_state(self.ctx.loop_vars)})")
            return
        if k == "Continue":
            if not self.ctx.in_loop:
                raise DecodeError("continue outside loop")
            self.add(f"pure (.cont {self.pack_state(self.ctx.loop_vars)})")
            return
        if k == "Return":
            self.emit_return(p)
            return
        if k == "Assign":
            target, value, _decl = p
            # place indices first, then RHS (spec §12)
            if target.kind != "Var":
                # for_read=False: Map/List slot stores must not get-then-set
                # (Map insert of a new key would KeyMissing on the get).
                _root, _get, setter = self.place_access(target, for_read=False)
                vval = self.bind_e(value)
                self.add(setter(vval))
            else:
                vval = self.bind_e(value)
                self.add(f"{mangle_value(target.name)} := {vval}")
            return
        if k == "TupleAssign":
            names, _decl, value = p
            vval = self.bind_e(value)
            if len(names) == 1:
                self.add(f"{mangle_value(names[0])} := {vval}")
            else:
                tmps = [self.ctx.gensym("ta") for _ in names]
                self.add(f"let ({', '.join(tmps)}) := {vval}")
                for nm, t in zip(names, tmps):
                    self.add(f"{mangle_value(nm)} := {t}")
            return
        if k == "Expr":
            c, pr = self.emit_expr(p)
            if not pr:
                self.add(c)
            else:
                self.add(f"let _ := {c}")
            return
        if k == "If":
            self.emit_if(p[0], p[1])
            return
        if k == "While":
            raise DecodeError("while is refused")
        if k == "ForRange":
            self.emit_for_range(p[0], p[1], p[2], p[3], p[4])
            return
        if k == "ForIn":
            self.emit_for_in(p[0], p[1], p[2])
            return
        if k == "Match":
            self.emit_match(p[0], p[1])
            return
        if k == "Assert":
            cond, line = p
            if cond.kind == "Binary" and cond.payload[0] == "Eq":
                _, lhs, rhs = cond.payload
                lval = self.bind_e(lhs)
                rval = self.bind_e(rhs)
                act = f"SudoRt.sudoAssertEq {lval} {rval} {line}"
            else:
                cval = self.bind_e(cond)
                act = f"SudoRt.sudoAssert {cval} {line}"
            if self.ctx.observe:
                self.add(f"if {self.ctx.caught}.isNone then")
                self.add(f"  match EStateM.run ({act}) () with")
                self.add("  | .ok _ _ => pure ()")
                self.add(f"  | .error __err _ => {self.ctx.caught} := some __err")
            else:
                self.add(act)
            return
        if k == "ExpectTrap":
            kind, body, line = p
            caught = self.ctx.gensym("caught")
            saved_obs, saved_c = self.ctx.observe, self.ctx.caught
            self.ctx.observe = True
            self.ctx.caught = caught
            self.add(f"let mut {caught} : Option SudoRt.Trap := none")
            self.emit_stmts(body)
            self.ctx.observe = saved_obs
            self.ctx.caught = saved_c
            self.add(f"match {caught} with")
            self.add(
                f"| none => SudoRt.trap \"AssertFailed\" s!\"line {line}: expected {kind}\""
            )
            self.add(
                f"| some __tr => if __tr.kind == {json.dumps(kind)} then pure () else throw __tr"
            )
            return
        raise DecodeError(f"unhandled stmt {k}")

    def emit_if(self, arms: list[tuple[Expr, list[Stmt]]], else_b: Optional[list[Stmt]]) -> None:
        def compile_arm(i: int) -> None:
            if i >= len(arms):
                if else_b:
                    self.emit_stmts(else_b)
                else:
                    self.add("pure ()")
                return
            cond, body = arms[i]
            cval = self.bind_e(cond)
            self.add(f"if {cval} then do")
            then_e = Emitter(self.ctx)
            then_e.emit_stmts(body)
            if not then_e.lines:
                then_e.add("pure ()")
            for ln in then_e.lines:
                self.add("  " + ln)
            self.add("else do")
            rest = Emitter(self.ctx)
            if i + 1 < len(arms) or else_b:
                rest.emit_if(arms[i + 1 :], else_b)
            else:
                rest.add("pure ()")
            if not rest.lines:
                rest.add("pure ()")
            for ln in rest.lines:
                self.add("  " + ln)

        compile_arm(0)

    def emit_match(self, scrut: Expr, arms: list[tuple[Any, list[Stmt]]]) -> None:
        sval = self.bind_e(scrut)
        self.add(f"match {sval} with")
        for pat, body in arms:
            inner = Emitter(self.ctx)
            inner.emit_stmts(body)
            if not inner.lines:
                inner.add("pure ()")
            pstr = self.render_pat(pat)
            if len(inner.lines) == 1:
                self.add(f"| {pstr} => {inner.lines[0]}")
            else:
                self.add(f"| {pstr} => do")
                for ln in inner.lines:
                    self.add("  " + ln)

    def render_pat(self, pat: Any) -> str:
        if pat[0] == "Int":
            return render_int(pat[1])
        if pat[0] == "Bool":
            return "true" if pat[1] else "false"
        if pat[0] == "Wildcard":
            return "_"
        if pat[0] == "Variant":
            _, en, vn, binders = pat
            bs = []
            for b in binders:
                bs.append("_" if b == "_" or b == "" else mangle_value(b))
            if en == "Option" and vn == "Some":
                return f"some {bs[0] if bs else '_'}"
            if en == "Option" and vn == "None":
                return "none"
            if en == "Result" and vn == "Ok":
                return f".ok {bs[0] if bs else '_'}"
            if en == "Result" and vn == "Err":
                return f".error {bs[0] if bs else '_'}"
            cn = qual_nominal(self.ctx, en, mangle_variant(en, vn))
            if not bs:
                return cn
            return cn + " " + " ".join(bs)
        raise DecodeError(f"pat {pat}")

    def emit_for_range(self, var: str, fr: Expr, to: Expr, down: bool, body: list[Stmt]) -> None:
        fval = self.bind_e(fr)
        tval = self.bind_e(to)
        written = sorted(written_in_stmts(body) | set(self.ctx.loop_vars))
        # keep existing loop vars plus newly written
        state_vars = sorted(set(written) | set(self.ctx.loop_vars))
        # exclude the loop index if it was somehow listed
        state_vars = [v for v in state_vars if v != var]
        if not self.ctx.locals:
            # fall back to written only
            pass
        st = self.pack_state(state_vars)
        iv = mangle_value(var)
        down_lit = "true" if down else "false"
        inner_ctx_loop_vars = self.ctx.loop_vars
        self.ctx.loop_vars = state_vars
        was_loop = self.ctx.in_loop
        self.ctx.in_loop = True
        inner = Emitter(self.ctx)
        # unpack state into mut copies? they're already mut in outer; lambda
        # takes st and we unpack then pack.
        if state_vars:
            inner.unpack_state(state_vars, "st", let_mut=True)
        inner.add(f"let {iv} := i")
        inner.emit_stmts(body, f"pure (.cont {inner.pack_state(state_vars)})")
        self.ctx.in_loop = was_loop
        self.ctx.loop_vars = inner_ctx_loop_vars
        body_do = self._do_block(inner.lines, "")
        # _do_block adds last ''; strip
        body_lines = [ln for ln in inner.lines if ln]
        body_do = "(fun i st => do\n  " + "\n  ".join(body_lines) + ")"
        step = self.bind_step(f"SudoRt.forRange {fval} {tval} {down_lit} {body_do} {st}", st)
        self._handle_step(step, state_vars)

    def emit_for_in(self, vars_: list[str], it: Expr, body: list[Stmt]) -> None:
        ival = self.bind_e(it)
        snap = self.ctx.gensym("it")
        # snapshot
        if it.ty[0] == "List":
            self.add(f"let {snap} := {ival}")
        elif it.ty[0] == "Set":
            self.add(f"let {snap} := SudoRt.setItems {ival}")
        elif it.ty[0] == "Map":
            self.add(f"let {snap} := ({ival}).pairs")
        else:
            raise DecodeError(f"for-in over {it.ty}")
        written = sorted(written_in_stmts(body) | set(self.ctx.loop_vars))
        state_vars = [v for v in sorted(set(written) | set(self.ctx.loop_vars)) if v not in vars_]
        st = self.pack_state(state_vars)
        saved = self.ctx.loop_vars
        was = self.ctx.in_loop
        self.ctx.loop_vars = state_vars
        self.ctx.in_loop = True
        inner = Emitter(self.ctx)
        if state_vars:
            inner.unpack_state(state_vars, "st", let_mut=True)
        if it.ty[0] == "Map" and len(vars_) == 2:
            inner.add(f"let ({mangle_value(vars_[0])}, {mangle_value(vars_[1])}) := item")
        elif len(vars_) == 1:
            inner.add(f"let {mangle_value(vars_[0])} := item")
        else:
            raise DecodeError("for-in arity")
        inner.emit_stmts(body, f"pure (.cont {inner.pack_state(state_vars)})")
        self.ctx.in_loop = was
        self.ctx.loop_vars = saved
        body_do = "(fun item st => do\n  " + "\n  ".join(ln for ln in inner.lines if ln) + ")"
        step = self.bind_step(f"SudoRt.forInArr {snap} {body_do} {st}", st)
        self._handle_step(step, state_vars)

    def _handle_step(self, step: str, state_vars: list[str]) -> None:
        self.add(f"match {step} with")
        if state_vars:
            self.add(f"| .cont st | .brk st => do")
            unpacker = Emitter(self.ctx)
            unpacker.unpack_state(state_vars, "st")
            for ln in unpacker.lines:
                self.add("  " + ln)
            self.add("  pure ()")
        else:
            self.add("| .cont _ | .brk _ => pure ()")
        if self.ctx.in_loop:
            self.add("| .ret r => pure (.ret r)")
        else:
            self.add("| .ret r => return r")


def collect_locals(params: list[Param], stmts: list[Stmt]) -> list[tuple[str, Any]]:
    """(name, ty) for every assigned/declared local, plus params."""
    seen: dict[str, Any] = {p.name: p.ty for p in params}

    def vis(s: Stmt) -> None:
        p = s.payload
        if s.kind == "Assign" and p[0].kind == "Var":
            seen.setdefault(p[0].name, p[1].ty)
        elif s.kind == "TupleAssign":
            names, _d, val = p
            if val.ty[0] == "Tuple":
                for n, t in zip(names, val.ty[1]):
                    seen.setdefault(n, t)
            else:
                for n in names:
                    seen.setdefault(n, val.ty)
        elif s.kind == "ForRange":
            seen.setdefault(p[0], ("Int",))
        elif s.kind == "ForIn":
            ity = p[1].ty
            names = p[0]
            if ity[0] == "List" and names:
                seen.setdefault(names[0], ity[1])
            elif ity[0] == "Set" and names:
                seen.setdefault(names[0], ity[1])
            elif ity[0] == "Map" and len(names) >= 2:
                seen.setdefault(names[0], ity[1])
                seen.setdefault(names[1], ity[2])
        elif s.kind == "Match":
            # Binders are introduced by the pattern, not pre-declared.
            pass

    walk_stmts(stmts, vis)
    return list(seen.items())


def default_value(ctx: Ctx, ty: Any) -> str:
    if ty is None:
        return "()"
    tag = ty[0]
    if tag == "Int":
        return "(0 : Int)"
    if tag == "Float":
        return "(0.0 : Float)"
    if tag == "Bool":
        return "false"
    if tag == "List":
        return f"(#[] : {render_ty(ctx, ty)})"
    if tag == "Set":
        return "{ items := #[] }"
    if tag == "Map":
        return "{ pairs := #[] }"
    if tag == "Option":
        return "none"
    if tag == "Result":
        # unused; Result locals are rare before assignment
        return f"(Except.error {default_value(ctx, ty[2])})"
    if tag == "Tuple":
        ts = ty[1]
        if not ts:
            return "()"
        return "(" + ", ".join(default_value(ctx, t) for t in ts) + ")"
    if tag == "Record":
        rec = None
        for m in ctx.all_mods:
            for r in m.records:
                if r.name == ty[1]:
                    rec = r
        tn = qual_nominal(ctx, ty[1], mangle_type(ty[1]))
        if rec is None:
            return f"(panic! \"uninhabited {tn}\")"
        fields = ", ".join(
            f"{mangle_field(ty[1], fn)} := {default_value(ctx, ft)}" for fn, ft in rec.fields
        )
        return f"{{ {fields} : {tn} }}"
    if tag == "Enum":
        en = None
        for m in ctx.all_mods:
            for e in m.enums:
                if e.name == ty[1]:
                    en = e
        if en and en.variants:
            vn, fields = en.variants[0]
            cn = qual_nominal(ctx, ty[1], mangle_variant(ty[1], vn))
            if not fields:
                return cn
            args = " ".join(default_value(ctx, ft) for _, ft in fields)
            return f"({cn} {args})"
        return f"(panic! \"uninhabited enum {ty[1]}\")"
    if tag == "Func":
        return f"(fun _ => pure {default_value(ctx, ty[2]) if ty[2] is not None else '()'})"
    raise DecodeError(f"no default for {ty}")


def emit_record_clean(ctx: Ctx, rec: Record) -> list[str]:
    tn = mangle_type(rec.name)
    lines = [f"structure {tn} where"]
    for fn, ft in rec.fields:
        lines.append(f"  {mangle_field(rec.name, fn)} : {render_ty(ctx, ft)}")
    lines.append("  deriving Inhabited, Repr")
    lines.append("")
    if rec.fields:
        eqs = " && ".join(
            f"(SudoRt.eq a.{mangle_field(rec.name, fn)} b.{mangle_field(rec.name, fn)})"
            for fn, _ in rec.fields
        )
    else:
        eqs = "true"
    lines.append(f"instance : SudoRt.SEq {tn} where")
    lines.append(f"  eq a b := {eqs}")
    lines.append("")
    if rec.fields:
        cmp_expr = ".eq"
        for fn, _ in reversed(rec.fields):
            fld = mangle_field(rec.name, fn)
            cmp_expr = f"(match SudoRt.cmp a.{fld} b.{fld} with | .eq => {cmp_expr} | o => o)"
    else:
        cmp_expr = ".eq"
    lines.append(f"instance : SudoRt.SOrd {tn} where")
    lines.append(f"  cmp a b := {cmp_expr}")
    lines.append("")
    if rec.fields:
        canons = ", ".join(
            f"SudoRt.canon r.{mangle_field(rec.name, fn)}" for fn, _ in rec.fields
        )
        body = (
            f"\"{{\\\"r\\\": \\\"{rec.name}\\\", \\\"v\\\": [\" ++ "
            f"String.intercalate \", \" [{canons}] ++ \"]}}\""
        )
    else:
        body = f"\"{{\\\"r\\\": \\\"{rec.name}\\\"}}\""
    lines.append(f"instance : SudoRt.Canon {tn} where")
    lines.append(f"  canon r := {body}")
    lines.append("")
    return lines


def emit_enum(ctx: Ctx, en: Enum) -> list[str]:
    tn = mangle_type(en.name)
    lines = [f"inductive {tn} where"]
    for vn, fields in en.variants:
        cn = mangle_variant(en.name, vn)
        if not fields:
            lines.append(f"  | {cn}")
        else:
            args = " ".join(f"({mangle_field(en.name + vn, fn)} : {render_ty(ctx, ft)})"
                            for fn, ft in fields)
            lines.append(f"  | {cn} {args}")
    lines.append("  deriving Inhabited, Repr")
    lines.append("")
    # SEq
    lines.append(f"instance : SudoRt.SEq {tn} where")
    lines.append("  eq")
    for vn, fields in en.variants:
        cn = mangle_variant(en.name, vn)
        if not fields:
            lines.append(f"    | .{cn}, .{cn} => true")
        else:
            xs = [f"x{i}" for i in range(len(fields))]
            ys = [f"y{i}" for i in range(len(fields))]
            cond = " && ".join(f"(SudoRt.eq {x} {y})" for x, y in zip(xs, ys))
            lines.append(f"    | .{cn} {' '.join(xs)}, .{cn} {' '.join(ys)} => {cond}")
    lines.append("    | _, _ => false")
    lines.append("")
    # SOrd by variant index
    lines.append(f"instance : SudoRt.SOrd {tn} where")
    lines.append("  cmp")
    for i, (vn, fields) in enumerate(en.variants):
        cn = mangle_variant(en.name, vn)
        for j, (vn2, fields2) in enumerate(en.variants):
            cn2 = mangle_variant(en.name, vn2)
            if i < j:
                xs = " ".join(["_"] * len(fields))
                ys = " ".join(["_"] * len(fields2))
                left = f".{cn}" + (f" {xs}" if fields else "")
                right = f".{cn2}" + (f" {ys}" if fields2 else "")
                lines.append(f"    | {left}, {right} => .lt")
            elif i > j:
                xs = " ".join(["_"] * len(fields))
                ys = " ".join(["_"] * len(fields2))
                left = f".{cn}" + (f" {xs}" if fields else "")
                right = f".{cn2}" + (f" {ys}" if fields2 else "")
                lines.append(f"    | {left}, {right} => .gt")
            else:
                if not fields:
                    lines.append(f"    | .{cn}, .{cn} => .eq")
                else:
                    xs = [f"x{k}" for k in range(len(fields))]
                    ys = [f"y{k}" for k in range(len(fields))]
                    cmp_expr = ".eq"
                    for x, y in reversed(list(zip(xs, ys))):
                        cmp_expr = f"(match SudoRt.cmp {x} {y} with | .eq => {cmp_expr} | o => o)"
                    lines.append(
                        f"    | .{cn} {' '.join(xs)}, .{cn} {' '.join(ys)} => {cmp_expr}"
                    )
    lines.append("")
    # Canon
    lines.append(f"instance : SudoRt.Canon {tn} where")
    lines.append("  canon")
    for vn, fields in en.variants:
        cn = mangle_variant(en.name, vn)
        label = f"{en.name}.{vn}"
        if not fields:
            lines.append(f"    | .{cn} => \"{{\\\"e\\\": \\\"{label}\\\"}}\"")
        else:
            xs = [f"x{k}" for k in range(len(fields))]
            canons = ", ".join(f"SudoRt.canon {x}" for x in xs)
            lines.append(
                f"    | .{cn} {' '.join(xs)} => "
                f"\"{{\\\"e\\\": \\\"{label}\\\", \\\"v\\\": [\" ++ "
                f"String.intercalate \", \" [{canons}] ++ \"]}}\""
            )
    lines.append("")
    return lines


def emit_const(ctx: Ctx, c: Const) -> list[str]:
    e = Emitter(ctx)
    code, pure = e.emit_expr(c.value)
    if e.lines:
        # constants must be foldable literals; if not, wrap in a def that runs SudoM
        body = e.lines + [f"pure {code}"]
        return [
            f"def {mangle_value(c.name)} : {render_ty(ctx, c.ty)} :=",
            f"  match EStateM.run (do",
            *[f"    {ln}" for ln in body],
            f"    ) () with",
            f"  | .ok v _ => v",
            f"  | .error t _ => panic! t.kind",
            "",
        ]
    return [
        f"def {mangle_value(c.name)} : {render_ty(ctx, c.ty)} := {code}",
        "",
    ]


def emit_func(ctx: Ctx, f: Func) -> list[str]:
    ctx = Ctx(
        module=ctx.module,
        all_mods=ctx.all_mods,
        inouts=[p.name for p in f.params if p.inout],
        locals=collect_locals(f.params, f.body),
        ret_ty=f.ret,
    )
    fn = mangle_value(f.name)
    params = " ".join(
        f"({mangle_value(p.name)} : {render_ty(ctx, p.ty)})" for p in f.params
    )
    ret = fret_ty_str(ctx, f)
    sig = f"def {fn} {params}: SudoRt.SudoM {ret} := do" if params else f"def {fn} : SudoRt.SudoM {ret} := do"
    e = Emitter(ctx)
    param_names = {p.name for p in f.params}
    for name, ty in ctx.locals:
        if name in param_names:
            e.add(f"let mut {mangle_value(name)} := {mangle_value(name)}")
        else:
            e.add(f"let mut {mangle_value(name)} := {default_value(ctx, ty)}")
    e.emit_stmts(f.body)
    if f.ret is None:
        pieces = [mangle_value(p.name) for p in f.params if p.inout]
        if not pieces:
            e.add("pure ()")
        elif len(pieces) == 1:
            e.add(f"pure {pieces[0]}")
        else:
            e.add("pure (" + ", ".join(pieces) + ")")
    lines = [sig] + [f"  {ln}" for ln in e.lines if ln] + [""]
    return lines


def emit_test_fn(ctx: Ctx, fn: str, t: Test) -> list[str]:
    ctx = Ctx(module=ctx.module, all_mods=ctx.all_mods, locals=collect_locals([], t.body))
    e = Emitter(ctx)
    for name, ty in ctx.locals:
        e.add(f"let mut {mangle_value(name)} := {default_value(ctx, ty)}")
    e.emit_stmts(t.body)
    e.add("pure ()")
    return [f"def {fn} : SudoRt.SudoM Unit := do"] + [f"  {ln}" for ln in e.lines if ln] + [""]


def emit_module_body(all_mods: list[Module], m: Module) -> list[str]:
    ctx = Ctx(module=m, all_mods=all_mods)
    lines: list[str] = [f"namespace {mangle_module(m.name)}", ""]
    for rec in m.records:
        lines.extend(emit_record_clean(ctx, rec))
    for en in m.enums:
        lines.extend(emit_enum(ctx, en))
    for c in m.consts:
        lines.extend(emit_const(ctx, c))
    for f in m.funcs:
        lines.extend(emit_func(ctx, f))
    lines.append(f"end {mangle_module(m.name)}")
    lines.append("")
    return lines


def emit_combined(runtime_src: str, req: EmitReq) -> str:
    parts = [
        "/- generated by the sudocode Lean backend (protocol 4). Lean 4.14. -/",
        "set_option linter.unusedVariables false",
        "",
    ]
    # Strip the runtime file's own module header options if we inline it.
    rt = runtime_src
    # Keep SudoRt as-is (it has its own set_option + namespace).
    parts.append(rt.rstrip())
    parts.append("")
    for m in req.modules:
        parts.extend(emit_module_body(req.modules, m))
    if req.with_tests:
        entry = req.modules[-1]
        ctx = Ctx(module=entry, all_mods=req.modules)
        names = test_fn_names(entry.tests)
        parts.append(f"open {mangle_module(entry.name)}")
        parts.append("")
        for fn, t in zip(names, entry.tests):
            parts.extend(emit_test_fn(ctx, fn, t))
        entries = ", ".join(f"({json.dumps(fn)}, {fn})" for fn in names)
        parts.append("def main : IO UInt32 :=")
        parts.append(f"  SudoRt.runTests [{entries}]")
        parts.append("")
    return "\n".join(parts) + "\n"


def emit_all(runtime_src: str, req: EmitReq) -> list[tuple[str, str]]:
    err = refuse_nontotal(req)
    if err:
        raise DecodeError(err)
    combined = emit_combined(runtime_src, req)
    files = [("SudoRt.lean", runtime_src)]
    # Per-module sources for proof use (import SudoRt; lake / LEAN_PATH).
    for m in req.modules:
        body = "\n".join(
            [
                f"import SudoRt",
                "set_option linter.unusedVariables false",
                "",
            ]
            + emit_module_body(req.modules, m)
        )
        files.append((f"{mangle_module(m.name)}.lean", body + "\n"))
    if req.with_tests:
        files.append((f"{req.entry}_test.lean", combined))
    return files


def respond_error(msg: str) -> None:
    json.dump({"error": msg}, sys.stdout, ensure_ascii=False)
    sys.stdout.write("\n")


def respond_ok(files: list[tuple[str, str]]) -> None:
    json.dump(
        {"files": [{"path": p, "contents": c} for p, c in files]},
        sys.stdout,
        ensure_ascii=False,
    )
    sys.stdout.write("\n")


def main() -> None:
    here = os.path.dirname(os.path.abspath(__file__))
    rt_path = os.path.join(here, "SudoRt.lean")
    try:
        with open(rt_path, encoding="utf-8") as f:
            runtime_src = f.read()
        raw = sys.stdin.read()
        val = json.loads(raw)
        req = decode_request(val)
        files = emit_all(runtime_src, req)
        respond_ok(files)
    except (DecodeError, json.JSONDecodeError) as e:
        respond_error(str(e))
    except Exception as e:
        respond_error(f"lean emitter: {type(e).__name__}: {e}")


if __name__ == "__main__":
    main()
