# The sudo Python runtime. Shipped alongside generated modules by sudoc.
# Implements the semantics pinned in spec/language.md: i64 wraparound, floor
# division with trapping, IEEE float edges, value-semantic deep copies, deep
# equality without Python's identity shortcut, and hashable key encodings so
# Lists (and anything structural) can be Map keys / Set elements.
from __future__ import annotations

import math
from dataclasses import dataclass, fields, is_dataclass
from typing import Any

_SIGN = 1 << 63
I64_MIN = -_SIGN
I64_MAX = _SIGN - 1


class SudoTrap(Exception):
    """A defined runtime fault (spec §8). Kind is one of the closed set."""

    def __init__(self, kind: str, detail: str = ""):
        self.kind = kind
        self.detail = detail
        super().__init__(f"{kind}{': ' + detail if detail else ''}")


def chk(x: int) -> int:
    """Trap Overflow when a result leaves the 64-bit range (spec §4.1)."""
    if not (I64_MIN <= x <= I64_MAX):
        raise SudoTrap("Overflow")
    return x


def div(a: int, b: int) -> int:
    if b == 0:
        raise SudoTrap("DivByZero")
    if a == I64_MIN and b == -1:
        raise SudoTrap("Overflow")
    return a // b  # Python // is floor division, as specified


def mod_i64(a: int, b: int) -> int:
    if b == 0:
        raise SudoTrap("DivByZero")
    return a % b  # Python % is floor modulo (sign of divisor), as specified


def abs_i64(x: int) -> int:
    return chk(abs(x))


def neg(x: int) -> int:
    return chk(-x)


def fdiv(a: float, b: float) -> float:
    if b == 0.0:
        if a == 0.0 or math.isnan(a):
            return math.nan
        return math.copysign(math.inf, a) * math.copysign(1.0, b)
    return a / b


def fmin(a: float, b: float) -> float:
    if math.isnan(a) or math.isnan(b):
        return math.nan
    if a == b:  # covers -0.0 vs 0.0: min picks the negative zero
        return a if math.copysign(1.0, a) < math.copysign(1.0, b) else b
    return a if a < b else b


def fmax(a: float, b: float) -> float:
    if math.isnan(a) or math.isnan(b):
        return math.nan
    if a == b:
        return a if math.copysign(1.0, a) > math.copysign(1.0, b) else b
    return a if a > b else b


def floor(x: float) -> float:
    if math.isnan(x) or math.isinf(x):
        return x
    return x // 1.0


def ceil(x: float) -> float:
    if math.isnan(x) or math.isinf(x):
        return x
    return -((-x) // 1.0)


def round_half_away(x: float) -> float:
    """Ties away from zero (spec §4.3), not Python's bankers' rounding.

    Truncates first, then compares the fractional distance to 0.5 —
    never adds 0.5 before rounding, which would double-round values
    just below a half boundary (e.g. 0.49999999999999994) up past
    the tie.
    """
    if math.isnan(x) or math.isinf(x):
        return x
    if x == 0.0:
        return x
    t = floor(x) if x >= 0 else ceil(x)
    d = abs(x - t)
    if d < 0.5:
        return t
    return t + math.copysign(1.0, x)


def sqrt(x: float) -> float:
    if math.isnan(x) or x < 0.0:
        return math.nan
    return math.sqrt(x)


def int_of(x: float) -> int:
    if math.isnan(x) or math.isinf(x):
        raise SudoTrap("InvalidConvert", "NaN or infinity to int")
    t = math.trunc(x)
    if t < I64_MIN or t > I64_MAX:
        raise SudoTrap("InvalidConvert", "float out of int range")
    return t


# ---- Option / Result -------------------------------------------------------


@dataclass
class Some:
    value: Any


class NoneOpt:
    __match_args__ = ()

    def __eq__(self, other):
        return isinstance(other, NoneOpt)

    def __repr__(self):
        return "None"


NONE = NoneOpt()


@dataclass
class Ok:
    value: Any


@dataclass
class Err:
    error: Any


def is_some(o) -> bool:
    return isinstance(o, Some)


def is_ok(r) -> bool:
    return isinstance(r, Ok)


def is_err(r) -> bool:
    return isinstance(r, Err)


def is_none(o) -> bool:
    return isinstance(o, NoneOpt)


def unwrap(o):
    if isinstance(o, Some):
        return o.value
    if isinstance(o, Ok):
        return o.value
    raise SudoTrap("UnwrapFailed")


def get_or(o, default):
    if isinstance(o, Some):
        return o.value
    if isinstance(o, Ok):
        return o.value
    return default


# ---- value semantics -------------------------------------------------------
#
# Every mutable composite (list including `text`, record, map, set) is a
# copy-on-write handle. `dup` is an O(1) share. A write forks that object
# if it has a second referent. Forking a parent (`_uniq`) `dup`s each
# child so their rc records the new alias — otherwise `b = a; b.append(x);
# b[0].append(y)` would mutate `a[0]`. Nested mutation of a still-shared
# parent also goes through `at_mut` / `field_mut` / `map_at_mut`. Tuples
# are immutable; Option/Result/enums have no in-place mutation path.


class _CowBox:
    """Shared backing. Slots beat a dict on the hot path."""

    __slots__ = ("d", "rc")

    def __init__(self, d, rc=1):
        self.d = d
        self.rc = rc


class CowList:
    """Copy-on-write list. Two wrappers may share one backing array."""

    __slots__ = ("_box",)

    def __init__(self, data=None, *, box=None):
        if box is not None:
            self._box = box
        elif isinstance(data, list):
            self._box = _CowBox(data)
        elif data is None:
            self._box = _CowBox([])
        else:
            self._box = _CowBox(list(data))

    def share(self):
        self._box.rc += 1
        return CowList(box=self._box)

    def _uniq(self):
        if self._box.rc > 1:
            self._box.rc -= 1
            # Share each child: two arrays now name them.
            self._box = _CowBox([dup(x) for x in self._box.d])

    def __len__(self):
        return len(self._box.d)

    def __iter__(self):
        return iter(self._box.d)

    def __add__(self, other):
        od = other._box.d if isinstance(other, CowList) else other
        return CowList([dup(x) for x in self._box.d] + [dup(x) for x in od])

    def __radd__(self, other):
        return CowList([dup(x) for x in other] + [dup(x) for x in self._box.d])

    def append(self, v):
        self._uniq()
        self._box.d.append(v)

    def __getitem__(self, i):
        return self._box.d[i]

    def __eq__(self, other):
        if isinstance(other, CowList):
            other = other._box.d
        if isinstance(other, list):
            return eq(self._box.d, other)
        return NotImplemented

    def __repr__(self):
        return f"CowList({self._box.d!r})"


def lst(xs=None):
    """Build a CowList, taking ownership of a fresh Python list."""
    if xs is None:
        return CowList()
    if isinstance(xs, CowList):
        return xs
    return CowList(xs if isinstance(xs, list) else list(xs))


class CowRec:
    """Copy-on-write record. `dup` is O(1) share; a field write forks."""

    __slots__ = ("_box",)

    def __init__(self, obj=None, *, box=None):
        if box is not None:
            object.__setattr__(self, "_box", box)
        else:
            object.__setattr__(self, "_box", _CowBox(obj))

    def _sudo_share(self):
        self._box.rc += 1
        return CowRec(box=self._box)

    def _sudo_uniq(self):
        if self._box.rc > 1:
            self._box.rc -= 1
            old = self._box.d
            cloned = type(old)(*(dup(getattr(old, f.name)) for f in fields(old)))
            object.__setattr__(self, "_box", _CowBox(cloned))

    def __getattr__(self, name):
        return getattr(self._box.d, name)

    def __setattr__(self, name, value):
        if name == "_box":
            object.__setattr__(self, name, value)
            return
        self._sudo_uniq()
        setattr(self._box.d, name, value)

    def __eq__(self, other):
        o = other._box.d if isinstance(other, CowRec) else other
        return eq(self._box.d, o)

    def __repr__(self):
        return f"CowRec({self._box.d!r})"


def rec(obj):
    """COW-wrap a generated record. Clones the dataclass so a pre-existing
    referent (host object, bare literal) is not aliased at rc=1. Enums
    pass through."""
    if isinstance(obj, CowRec):
        return obj
    kind = getattr(type(obj), "_sudo_kind", None)
    if kind and kind[0] == "r":
        cloned = type(obj)(*(dup(getattr(obj, f.name)) for f in fields(obj)))
        return CowRec(cloned)
    return obj


def out_record(v):
    """Host out: a deep snapshot, not a COW alias.

    Records become a fresh dataclass. List/text/map/set fields become a
    new handle whose children are snapshotted the same way, so a host
    write through `__getitem__` / field access cannot leak into the
    callee (lockstep.md §5.3). One-level `share()` is not enough.
    """
    return _out_value(v)


def _out_value(v):
    if isinstance(v, CowRec):
        v = v._box.d
    if isinstance(v, Some):
        return Some(_out_value(v.value))
    if isinstance(v, Ok):
        return Ok(_out_value(v.value))
    if isinstance(v, Err):
        return Err(_out_value(v.error))
    if isinstance(v, NoneOpt):
        return v
    kind = getattr(type(v), "_sudo_kind", None)
    if kind and kind[0] in ("r", "e"):
        return type(v)(*(_out_value(getattr(v, f.name)) for f in fields(v)))
    if isinstance(v, CowList):
        return CowList([_out_value(x) for x in v._box.d])
    if isinstance(v, tuple):
        return tuple(_out_value(x) for x in v)
    name = type(v).__name__
    if name == "SudoMap":
        out = type(v)()
        for k, val in v.pairs():
            out[k] = _out_value(val)
        return out
    if name == "SudoSet":
        out = type(v)()
        for val in v.items_list():
            out.add(_out_value(val))
        return out
    return v


def field_mut(obj, name):
    """Field as a mutation receiver. Fork the record if shared; if it had
    a second referent, `dup` the field so the child's rc records the alias."""
    obj = obj if isinstance(obj, CowRec) else rec(obj)
    shared = obj._box.rc > 1
    obj._sudo_uniq()
    inner = obj._box.d
    if shared:
        setattr(inner, name, dup(getattr(inner, name)))
    return getattr(inner, name)


def _elems(a):
    return a._box.d if isinstance(a, CowList) else a


# Copy-volume counters for the complexity harness. Off unless the harness
# calls reset_dup_stats() — programs that are not measuring do not pay.
_DUP_COUNTING = False
_DUP_STATS = {"list": 0, "leaves": 0, "list_by_len": {}, "tuple": 0}


def reset_dup_stats():
    global _DUP_COUNTING
    _DUP_COUNTING = True
    _DUP_STATS["list"] = 0
    _DUP_STATS["leaves"] = 0
    _DUP_STATS["list_by_len"] = {}
    _DUP_STATS["tuple"] = 0


def dup_stats():
    return {
        "list": _DUP_STATS["list"],
        "leaves": _DUP_STATS["leaves"],
        "list_by_len": dict(_DUP_STATS["list_by_len"]),
        "tuple": _DUP_STATS["tuple"],
    }


def _count_list_dup(n: int) -> None:
    if not _DUP_COUNTING:
        return
    _DUP_STATS["list"] += 1
    d = _DUP_STATS["list_by_len"]
    d[n] = d.get(n, 0) + 1


def dup(v):
    """O(1) share for lists, maps, sets, and records."""
    if isinstance(v, CowList):
        _count_list_dup(len(v))
        return v.share()
    if isinstance(v, CowRec):
        return v._sudo_share()
    if isinstance(v, list):
        _count_list_dup(len(v))
        return CowList([dup(x) for x in v])
    if isinstance(v, tuple):
        if _DUP_COUNTING:
            _DUP_STATS["tuple"] += 1
        return tuple(dup(x) for x in v)
    if isinstance(v, SudoMap):
        return v.share()
    if isinstance(v, SudoSet):
        return v.share()
    if isinstance(v, Some):
        return Some(dup(v.value))
    if isinstance(v, Ok):
        return Ok(dup(v.value))
    if isinstance(v, Err):
        return Err(dup(v.error))
    kind = getattr(type(v), "_sudo_kind", None)
    if kind and kind[0] == "r":
        return rec(v)
    if is_dataclass(v):
        cls = type(v)
        return cls(*(dup(getattr(v, f.name)) for f in fields(v)))
    if _DUP_COUNTING and (isinstance(v, (int, float, bool)) or v is None):
        _DUP_STATS["leaves"] += 1
    return v


def eq(a, b) -> bool:
    """Deep structural equality with IEEE float semantics (NaN != NaN).

    Python's list equality short-circuits on identity, which would make
    [nan] == [nan] true; this walks structures explicitly instead.
    """
    if isinstance(a, float) or isinstance(b, float):
        return isinstance(a, float) and isinstance(b, float) and a == b
    if isinstance(a, bool) or isinstance(b, bool):
        return a is b
    if isinstance(a, int) and isinstance(b, int):
        return a == b
    if isinstance(a, CowRec):
        a = a._box.d
    if isinstance(b, CowRec):
        b = b._box.d
    if isinstance(a, CowList):
        a = a._box.d
    if isinstance(b, CowList):
        b = b._box.d
    if isinstance(a, (list, tuple)) and isinstance(b, (list, tuple)):
        return len(a) == len(b) and all(eq(x, y) for x, y in zip(a, b))
    if isinstance(a, SudoMap) and isinstance(b, SudoMap):
        if len(a) != len(b):
            return False
        for k, v in a.pairs():
            other = b.get_opt(k)
            if isinstance(other, NoneOpt) or not eq(v, other.value):
                return False
        return True
    if isinstance(a, SudoSet) and isinstance(b, SudoSet):
        return len(a) == len(b) and all(x in b for x in a.items_list())
    if isinstance(a, NoneOpt) and isinstance(b, NoneOpt):
        return True
    if is_dataclass(a) and is_dataclass(b):
        if type(a) is not type(b):
            return False
        return all(eq(getattr(a, f.name), getattr(b, f.name)) for f in fields(a))
    return False


def key_form(v):
    """Immutable, hashable encoding of a (hashable-typed) sudo value."""
    if isinstance(v, CowRec):
        v = v._box.d
    if isinstance(v, CowList):
        return tuple(key_form(x) for x in v._box.d)
    if isinstance(v, list):
        return tuple(key_form(x) for x in v)
    if isinstance(v, tuple):
        return tuple(key_form(x) for x in v)
    if isinstance(v, Some):
        return ("Some", key_form(v.value))
    if isinstance(v, NoneOpt):
        return ("None",)
    if isinstance(v, Ok):
        return ("Ok", key_form(v.value))
    if isinstance(v, Err):
        return ("Err", key_form(v.error))
    if is_dataclass(v):
        return (type(v).__name__,) + tuple(key_form(getattr(v, f.name)) for f in fields(v))
    return v


# ---- containers ------------------------------------------------------------


def at(a, i: int):
    d = _elems(a)
    if not 0 <= i < len(d):
        raise SudoTrap("OutOfBounds", f"index {i} of length {len(d)}")
    return d[i]


def at_mut(a, i: int):
    """Index as a mutation receiver. Fork the parent if shared; if the
    parent had a second referent, `dup` the slot so the child's rc
    records the alias before the write."""
    shared = isinstance(a, CowList) and a._box.rc > 1
    if isinstance(a, CowList):
        a._uniq()
        d = a._box.d
    else:
        d = a
    if not 0 <= i < len(d):
        raise SudoTrap("OutOfBounds", f"index {i} of length {len(d)}")
    if shared:
        d[i] = dup(d[i])
    return d[i]


def put(a, i: int, v):
    if isinstance(a, CowList):
        a._uniq()
        d = a._box.d
    else:
        d = a
    if not 0 <= i < len(d):
        raise SudoTrap("OutOfBounds", f"index {i} of length {len(d)}")
    d[i] = v


def map_put(m, k, v):
    """Map insert-or-overwrite with call-arg evaluation order.

    Python's `m[k] = v` evaluates the RHS before the key; call arguments are
    left-to-right, so the key traps before the value (§12 place-before-RHS).
    """
    m[k] = v


def pop(a):
    if isinstance(a, CowList):
        a._uniq()
        d = a._box.d
    else:
        d = a
    if not d:
        raise SudoTrap("OutOfBounds", "pop from empty list")
    return d.pop()


def insert(a, i: int, v):
    if isinstance(a, CowList):
        a._uniq()
        d = a._box.d
    else:
        d = a
    if not 0 <= i <= len(d):
        raise SudoTrap("OutOfBounds", f"insert at {i} of length {len(d)}")
    d.insert(i, v)


def remove_at(a, i: int):
    if isinstance(a, CowList):
        a._uniq()
        d = a._box.d
    else:
        d = a
    if not 0 <= i < len(d):
        raise SudoTrap("OutOfBounds", f"remove_at {i} of length {len(d)}")
    return d.pop(i)


def swap(a, i: int, j: int):
    if isinstance(a, CowList):
        a._uniq()
        d = a._box.d
    else:
        d = a
    if not (0 <= i < len(d) and 0 <= j < len(d)):
        raise SudoTrap("OutOfBounds", f"swap {i},{j} of length {len(d)}")
    d[i], d[j] = d[j], d[i]


# ---- operation counting (complexity harness only) -----------------------
#
# Dead code in every normal build: backend_py only emits calls to these
# when SUDO_COUNT_OPS was set in the *codegen* process's environment (an
# opt-in read once per `sudoc build` invocation -- see backend_py/src/lib.rs
# Emitter.count_ops). sudo has no globals and no sudo program can observe
# this state; it exists purely for tools/complexity.bzl's Python drivers to
# import _sudo_rt and inspect after calling into generated code.

_OP_COUNTS = {"add": 0, "append": 0}


def count_add(l, r):
    """Instrumented replacement for `l + r` (list/text concatenation):
    counts elements touched, CPython's real O(len(l)+len(r)) cost for
    list concatenation."""
    _OP_COUNTS["add"] += len(l) + len(r)
    return l + r


def count_append(a, v) -> None:
    """Instrumented replacement for `a.append(v)`."""
    _OP_COUNTS["append"] += 1
    a.append(v)


def op_counts() -> dict:
    return dict(_OP_COUNTS)


def reset_op_counts() -> None:
    for k in _OP_COUNTS:
        _OP_COUNTS[k] = 0


def _sort_key(x):
    if isinstance(x, float):
        if math.isnan(x):
            return (2, 0.0, 0.0)
        return (1, x, math.copysign(1.0, x))
    return (1, x, 0.0)


def sort(a):
    """Ascending stable sort; floats order NaN last, -0.0 before 0.0."""
    if isinstance(a, CowList):
        a._uniq()
        a._box.d.sort(key=_sort_key)
    else:
        a.sort(key=_sort_key)


def filled(n: int, v):
    if n < 0:
        raise SudoTrap("InvalidArg", f"filled({n})")
    return CowList([dup(v) for _ in range(n)])


def text(s: str):
    """Text literal: list of Unicode scalar values."""
    return CowList([ord(c) for c in s])


def text_str(v) -> str:
    """Boundary helper: scalar list back to a host string."""
    return "".join(chr(c) for c in _elems(v))


class SudoMap:
    """Insertion-ordered in practice (dict-backed) — order is unspecified by
    the language. Keys are stored by structural key_form so Lists and records
    can be keys; original key values are retained for iteration. Copy-on-write
    like CowList: `dup` is a share; a write forks the dict."""

    def __init__(self, *, box=None):
        self._box = box if box is not None else _CowBox({})

    def share(self):
        self._box.rc += 1
        return SudoMap(box=self._box)

    def _uniq(self):
        if self._box.rc > 1:
            self._box.rc -= 1
            # Keys and values are children: share each so a later write
            # through one map cannot leak into the other.
            self._box = _CowBox(
                {k: (dup(kk), dup(vv)) for k, (kk, vv) in self._box.d.items()}
            )

    @property
    def _d(self):
        return self._box.d

    def __len__(self):
        return len(self._d)

    def __contains__(self, k):
        return key_form(k) in self._d

    def __getitem__(self, k):
        kf = key_form(k)
        if kf not in self._d:
            raise SudoTrap("KeyMissing")
        return self._d[kf][1]

    def __setitem__(self, k, v):
        self._uniq()
        self._d[key_form(k)] = (dup(k), v)

    def get_opt(self, k):
        kf = key_form(k)
        if kf in self._d:
            return Some(self._d[kf][1])
        return NONE

    def delete(self, k) -> bool:
        self._uniq()
        kf = key_form(k)
        if kf in self._d:
            del self._d[kf]
            return True
        return False

    def keys_list(self):
        return CowList([dup(k) for k, _ in self._d.values()])

    def values_list(self):
        return CowList([dup(v) for _, v in self._d.values()])

    def pairs(self) -> list:
        return [(k, v) for k, v in self._d.values()]

    def __eq__(self, other):
        return isinstance(other, SudoMap) and eq(self, other)

    def __repr__(self):
        inner = ", ".join(f"{k!r}: {v!r}" for k, v in self._d.values())
        return "{" + inner + "}"


def map_at_mut(m, k):
    """Map lookup as a mutation receiver. Same rule as `at_mut`."""
    shared = m._box.rc > 1
    m._uniq()
    kf = key_form(k)
    if kf not in m._d:
        raise SudoTrap("KeyMissing")
    if shared:
        old_k, old_v = m._d[kf]
        m._d[kf] = (old_k, dup(old_v))
    return m._d[kf][1]


class SudoSet:
    def __init__(self, *, box=None):
        self._box = box if box is not None else _CowBox({})

    def share(self):
        self._box.rc += 1
        return SudoSet(box=self._box)

    def _uniq(self):
        if self._box.rc > 1:
            self._box.rc -= 1
            self._box = _CowBox({k: dup(v) for k, v in self._box.d.items()})

    @property
    def _d(self):
        return self._box.d

    def __len__(self):
        return len(self._d)

    def __contains__(self, v):
        return key_form(v) in self._d

    def add(self, v) -> bool:
        self._uniq()
        kf = key_form(v)
        if kf in self._d:
            return False
        self._d[kf] = dup(v)
        return True

    def remove(self, v) -> bool:
        self._uniq()
        kf = key_form(v)
        if kf in self._d:
            del self._d[kf]
            return True
        return False

    def items_list(self):
        return CowList([dup(v) for v in self._d.values()])

    def __eq__(self, other):
        return isinstance(other, SudoSet) and eq(self, other)

    def __repr__(self):
        return "{" + ", ".join(repr(v) for v in self._d.values()) + "}"


# ---- tests -----------------------------------------------------------------


def sudo_assert(cond: bool, line: int):
    if not cond:
        raise SudoTrap("AssertFailed", f"line {line}")


def canon(v) -> str:
    """Canonical display serialization (lockstep.md §4). Diagnostic-only:
    Map/Set entries appear in this target's iteration order on purpose —
    it shows exactly what this implementation saw."""
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, float):
        if math.isnan(v):
            s = "NaN"
        elif math.isinf(v):
            s = "Inf" if v > 0 else "-Inf"
        else:
            s = repr(v)
        return '{"f": "%s"}' % s
    if isinstance(v, CowRec):
        v = v._box.d
    if isinstance(v, CowList):
        return "[" + ", ".join(canon(x) for x in v._box.d) + "]"
    if isinstance(v, (list, tuple)):
        return "[" + ", ".join(canon(x) for x in v) + "]"
    if isinstance(v, SudoMap):
        pairs = ", ".join("[%s, %s]" % (canon(k), canon(x)) for k, x in v.pairs())
        return '{"m": [' + pairs + "]}"
    if isinstance(v, SudoSet):
        return '{"s": [' + ", ".join(canon(x) for x in v.items_list()) + "]}"
    if isinstance(v, Some):
        return '{"e": "Option.Some", "v": [%s]}' % canon(v.value)
    if isinstance(v, NoneOpt):
        return '{"e": "Option.None"}'
    if isinstance(v, Ok):
        return '{"e": "Result.Ok", "v": [%s]}' % canon(v.value)
    if isinstance(v, Err):
        return '{"e": "Result.Err", "v": [%s]}' % canon(v.error)
    if is_dataclass(v):
        kind, name = getattr(type(v), "_sudo_kind", ("r", type(v).__name__))
        vals = ", ".join(canon(getattr(v, f.name)) for f in fields(v))
        if vals:
            return '{"%s": "%s", "v": [%s]}' % (kind, name, vals)
        return '{"%s": "%s"}' % (kind, name)
    return repr(v)


def sudo_assert_eq(l, r, line: int):
    if not eq(l, r):
        raise SudoTrap("AssertFailed", f"line {line}: {canon(l)} != {canon(r)}")


def run_tests(globals_dict) -> int:
    """Run every test_* function; print TAP-ish lines; return exit code."""
    tests = [
        (name, fn)
        for name, fn in globals_dict.items()
        if name.startswith("test_") and callable(fn)
    ]
    failures = 0
    for i, (name, fn) in enumerate(tests, 1):
        try:
            fn()
            print(f"ok {i} - {name}")
        except SudoTrap as t:
            failures += 1
            print(f"not ok {i} - {name} [{t.kind}{': ' + t.detail if t.detail else ''}]")
        except RecursionError:
            failures += 1
            print(f"not ok {i} - {name} [StackOverflow]")
    print(f"# {len(tests) - failures}/{len(tests)} passed")
    return 1 if failures else 0


# ---- host boundary (lockstep.md §5.1) --------------------------------------


class SudoError(Exception):
    """A sudo Result Err surfaced to the host."""

    def __init__(self, payload):
        self.payload = payload
        super().__init__(str(payload))


def host_int(x) -> int:
    if isinstance(x, bool) or not isinstance(x, int):
        raise ValueError(f"expected int, got {type(x).__name__}")
    if not (I64_MIN <= x <= I64_MAX):
        raise ValueError("int out of 64-bit range")
    return x


def host_float(x) -> float:
    if isinstance(x, bool) or not isinstance(x, (int, float)):
        raise ValueError(f"expected float, got {type(x).__name__}")
    return float(x)


def host_bool(x) -> bool:
    if not isinstance(x, bool):
        raise ValueError(f"expected bool, got {type(x).__name__}")
    return x


def host_text(x):
    if not isinstance(x, str):
        raise ValueError(f"expected str, got {type(x).__name__}")
    return CowList([ord(c) for c in x])


def host_list(x):
    if isinstance(x, (str, bytes)) or not hasattr(x, "__iter__"):
        raise ValueError(f"expected a sequence, got {type(x).__name__}")
    return CowList(list(x))


def host_tuple(x, n: int) -> tuple:
    t = tuple(x)
    if len(t) != n:
        raise ValueError(f"expected a {n}-tuple, got length {len(t)}")
    return t


def host_map(x, kconv, vconv):
    m = SudoMap()
    for k, v in x.items():
        m[kconv(k)] = vconv(v)
    return m


def host_set(x, conv):
    s = SudoSet()
    for v in x:
        s.add(conv(v))
    return s


def out_option(o, conv):
    return None if isinstance(o, NoneOpt) else conv(o.value)


def out_result(r, okconv, errconv):
    if isinstance(r, Ok):
        return okconv(r.value)
    raise SudoError(errconv(r.error))


def _hashable(k):
    if isinstance(k, CowList):
        return tuple(_hashable(v) for v in k._box.d)
    if isinstance(k, list):
        return tuple(_hashable(v) for v in k)
    return k


def out_map(m, kconv, vconv) -> dict:
    return {_hashable(kconv(k)): vconv(v) for k, v in m.pairs()}


def out_set(s, conv) -> set:
    return {_hashable(conv(v)) for v in s.items_list()}


def writeback_list(host: list, new: list, conv):
    host[:] = [conv(v) for v in new]


def writeback_map(host: dict, new, kconv, vconv):
    host.clear()
    host.update(out_map(new, kconv, vconv))


def writeback_set(host: set, new, conv):
    host.clear()
    host.update(out_set(new, conv))
