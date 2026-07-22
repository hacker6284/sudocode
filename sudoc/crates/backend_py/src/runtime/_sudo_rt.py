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
    """Ties away from zero (spec §4.3), not Python's bankers' rounding."""
    if math.isnan(x) or math.isinf(x):
        return x
    if x == 0.0:
        return x
    if x > 0:
        return floor(x + 0.5)
    return ceil(x - 0.5)


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


def dup(v):
    """Deep copy per sudo value semantics. Scalars are immutable in Python."""
    if isinstance(v, list):
        return [dup(x) for x in v]
    if isinstance(v, tuple):
        return tuple(dup(x) for x in v)
    if isinstance(v, SudoMap):
        return v._dup()
    if isinstance(v, SudoSet):
        return v._dup()
    if isinstance(v, Some):
        return Some(dup(v.value))
    if isinstance(v, Ok):
        return Ok(dup(v.value))
    if isinstance(v, Err):
        return Err(dup(v.error))
    if is_dataclass(v):
        cls = type(v)
        return cls(*(dup(getattr(v, f.name)) for f in fields(v)))
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


def at(a: list, i: int):
    if not 0 <= i < len(a):
        raise SudoTrap("OutOfBounds", f"index {i} of length {len(a)}")
    return a[i]


def put(a: list, i: int, v):
    if not 0 <= i < len(a):
        raise SudoTrap("OutOfBounds", f"index {i} of length {len(a)}")
    a[i] = v


def pop(a: list):
    if not a:
        raise SudoTrap("OutOfBounds", "pop from empty list")
    return a.pop()


def insert(a: list, i: int, v):
    if not 0 <= i <= len(a):
        raise SudoTrap("OutOfBounds", f"insert at {i} of length {len(a)}")
    a.insert(i, v)


def remove_at(a: list, i: int):
    if not 0 <= i < len(a):
        raise SudoTrap("OutOfBounds", f"remove_at {i} of length {len(a)}")
    return a.pop(i)


def swap(a: list, i: int, j: int):
    if not (0 <= i < len(a) and 0 <= j < len(a)):
        raise SudoTrap("OutOfBounds", f"swap {i},{j} of length {len(a)}")
    a[i], a[j] = a[j], a[i]


def _sort_key(x):
    if isinstance(x, float):
        if math.isnan(x):
            return (2, 0.0, 0.0)
        return (1, x, math.copysign(1.0, x))
    return (1, x, 0.0)


def sort(a: list):
    """Ascending stable sort; floats order NaN last, -0.0 before 0.0."""
    a.sort(key=_sort_key)


def filled(n: int, v) -> list:
    if n < 0:
        raise SudoTrap("InvalidArg", f"filled({n})")
    return [dup(v) for _ in range(n)]


def text(s: str) -> list:
    """Text literal: list of Unicode scalar values."""
    return [ord(c) for c in s]


def text_str(v: list) -> str:
    """Boundary helper: scalar list back to a host string."""
    return "".join(chr(c) for c in v)


class SudoMap:
    """Insertion-ordered in practice (dict-backed) — order is unspecified by
    the language. Keys are stored by structural key_form so Lists and records
    can be keys; original key values are retained for iteration."""

    def __init__(self):
        self._d: dict = {}

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
        self._d[key_form(k)] = (dup(k), v)

    def get_opt(self, k):
        kf = key_form(k)
        if kf in self._d:
            return Some(self._d[kf][1])
        return NONE

    def delete(self, k) -> bool:
        kf = key_form(k)
        if kf in self._d:
            del self._d[kf]
            return True
        return False

    def keys_list(self) -> list:
        return [dup(k) for k, _ in self._d.values()]

    def values_list(self) -> list:
        return [v for _, v in self._d.values()]

    def pairs(self) -> list:
        return [(k, v) for k, v in self._d.values()]

    def _dup(self):
        m = SudoMap()
        for k, v in self._d.values():
            m._d[key_form(k)] = (dup(k), dup(v))
        return m

    def __eq__(self, other):
        return isinstance(other, SudoMap) and eq(self, other)

    def __repr__(self):
        inner = ", ".join(f"{k!r}: {v!r}" for k, v in self._d.values())
        return "{" + inner + "}"


class SudoSet:
    def __init__(self):
        self._d: dict = {}

    def __len__(self):
        return len(self._d)

    def __contains__(self, v):
        return key_form(v) in self._d

    def add(self, v) -> bool:
        kf = key_form(v)
        if kf in self._d:
            return False
        self._d[kf] = dup(v)
        return True

    def remove(self, v) -> bool:
        kf = key_form(v)
        if kf in self._d:
            del self._d[kf]
            return True
        return False

    def items_list(self) -> list:
        return [dup(v) for v in self._d.values()]

    def _dup(self):
        s = SudoSet()
        s._d = {k: dup(v) for k, v in self._d.items()}
        return s

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


def host_text(x) -> list:
    if not isinstance(x, str):
        raise ValueError(f"expected str, got {type(x).__name__}")
    return [ord(c) for c in x]


def host_list(x) -> list:
    if isinstance(x, (str, bytes)) or not hasattr(x, "__iter__"):
        raise ValueError(f"expected a sequence, got {type(x).__name__}")
    return list(x)


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
