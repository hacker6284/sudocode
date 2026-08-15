//! sudo runtime for the Zig backend (Zig 0.16).
//!
//! Trap surface: a closed error set; sudo functions return `SudoError!T` and
//! propagate with `try`; `expect_trap` observes an error locally. Trap kinds
//! map 1:1 onto sudo's spec §8 set, so `@errorName` is the reported kind.
//!
//! Memory: no process-global arena. Each sudo function opens a per-call
//! scratch arena over `backing()`; each test opens a per-test arena for
//! module consts and top-level return values. Both are `defer`-freed.
//! In Debug builds, `backing()` is a `DebugAllocator` so use-after-free and
//! leaks surface as nonzero exit from `run_tests` via `backingReport`.

const std = @import("std");

pub const SudoError = error{
    OutOfBounds,
    KeyMissing,
    DivByZero,
    Overflow,
    UnwrapFailed,
    InvalidConvert,
    InvalidArg,
    AssertFailed,
};

/// Re-export so generated modules (which import only `rt`, never `std`) can
/// name the allocator type in the explicit `alloc` params they now thread.
pub const Allocator = std.mem.Allocator;
pub const ArenaAllocator = std.heap.ArenaAllocator;

/// Debug-mode leak / double-free / large-allocation-UAF oracle; only consulted
/// when `builtin.mode == .Debug`. NOTE: this does NOT catch every
/// use-after-reset — a stale read of a small, still-mapped freed region before
/// the next allocation reuses it returns valid-looking bytes. Treat a clean
/// Debug run as strong evidence, not a lifetime proof.
var sudo_dbg: std.heap.DebugAllocator(.{}) = .init;

/// Reset a per-loop arena between iterations. In Debug we `.free_all` so the
/// iteration's memory is actually returned to the backing DebugAllocator
/// (making some use-after-reset bugs faultable); in release we
/// `.retain_capacity` for speed.
pub fn loopReset(a: *ArenaAllocator) void {
    _ = a.reset(if (@import("builtin").mode == .Debug) .free_all else .retain_capacity);
}

/// Real freeing backing for per-call scratch and per-test arenas. In Debug,
/// routes through `DebugAllocator` so leaks and use-after-free are detected.
pub fn backing() std.mem.Allocator {
    return if (@import("builtin").mode == .Debug) sudo_dbg.allocator() else std.heap.page_allocator;
}

/// Call once after all tests. In Debug, reports leaks (nonzero return on leak).
pub fn backingReport() u8 {
    if (@import("builtin").mode == .Debug) {
        if (sudo_dbg.deinit() == .leak) return 1;
    }
    return 0;
}

/// Arena-allocate a single value (used when boxing enum/Option/Result payloads).
pub fn box(alloc: std.mem.Allocator, comptime T: type, v: T) *const T {
    const p = alloc.create(T) catch @panic("sudo: arena OOM");
    p.* = v;
    return p;
}

// ── Checked i64 arithmetic ──────────────────────────────────────────────────

pub fn add(a: i64, b: i64) SudoError!i64 {
    return std.math.add(i64, a, b) catch SudoError.Overflow;
}

pub fn sub(a: i64, b: i64) SudoError!i64 {
    return std.math.sub(i64, a, b) catch SudoError.Overflow;
}

pub fn mul(a: i64, b: i64) SudoError!i64 {
    return std.math.mul(i64, a, b) catch SudoError.Overflow;
}

pub fn neg(a: i64) SudoError!i64 {
    return std.math.negate(a) catch SudoError.Overflow;
}

pub fn absInt(a: i64) SudoError!i64 {
    if (a == std.math.minInt(i64)) return SudoError.Overflow;
    return if (a < 0) -a else a;
}

/// Floor division. Guards zero divisor and `minInt / -1` overflow (both are
/// uncatchable panics if `@divFloor` is called raw).
pub fn divFloor(a: i64, b: i64) SudoError!i64 {
    if (b == 0) return SudoError.DivByZero;
    if (b == -1) return neg(a);
    return @divFloor(a, b);
}

/// Floor modulo. Same zero-divisor guard; `a mod -1 == 0` for all a.
pub fn modFloor(a: i64, b: i64) SudoError!i64 {
    if (b == 0) return SudoError.DivByZero;
    if (b == -1) return 0;
    return @mod(a, b);
}

// ── Float helpers ───────────────────────────────────────────────────────────

/// NaN-propagating min; prefers -0.0 over +0.0 when equal (unlike C fmin).
pub fn fmin(a: f64, b: f64) f64 {
    if (std.math.isNan(a) or std.math.isNan(b)) return std.math.nan(f64);
    if (a == b) return if (std.math.signbit(a)) a else b;
    return if (a < b) a else b;
}

/// NaN-propagating max; prefers +0.0 over -0.0 when equal (unlike C fmax).
pub fn fmax(a: f64, b: f64) f64 {
    if (std.math.isNan(a) or std.math.isNan(b)) return std.math.nan(f64);
    if (a == b) return if (std.math.signbit(a)) b else a;
    return if (a > b) a else b;
}

/// `@abs` on f64 yields f64 (unlike int `@abs`, which yields unsigned).
pub fn absFloat(x: f64) f64 {
    return @abs(x);
}

/// Float division: IEEE (±Inf / NaN on zero divisor), never a trap. Routed
/// through a runtime function so Zig does not reject a comptime `0.0 / 0.0`
/// ("division by zero here causes illegal behavior") — runtime f64 params make
/// it a normal IEEE operation.
pub fn fdiv(a: f64, b: f64) f64 {
    return a / b;
}

/// Float +, -, * routed through runtime f64 params for the same reason as
/// `fdiv`: emitting `(0.1 + 0.2)` inline would let Zig fold the literals at
/// `comptime_float` (extended) precision and only then coerce to f64, giving
/// exactly 0.3 — diverging from every other backend's IEEE binary64 per-op
/// rounding (spec §4.2). Runtime f64 params force real f64 arithmetic.
pub fn fadd(a: f64, b: f64) f64 {
    return a + b;
}

pub fn fsub(a: f64, b: f64) f64 {
    return a - b;
}

pub fn fmul(a: f64, b: f64) f64 {
    return a * b;
}

pub fn floor(x: f64) f64 {
    return @floor(x);
}

pub fn ceil(x: f64) f64 {
    return @ceil(x);
}

/// Zig 0.16 `std.math.round` is ties-away-from-zero (verified).
pub fn round(x: f64) f64 {
    return std.math.round(x);
}

/// `sqrt(-1.0)` returns NaN natively; no guard needed.
pub fn sqrt(x: f64) f64 {
    return std.math.sqrt(x);
}

pub fn floatOfInt(i: i64) f64 {
    return @floatFromInt(i);
}

pub fn nan() f64 {
    return std.math.nan(f64);
}

pub fn inf() f64 {
    return std.math.inf(f64);
}

/// Truncating float→int with InvalidConvert on NaN/Inf/out-of-i64-range.
pub fn intOfFloat(f: f64) SudoError!i64 {
    if (std.math.isNan(f)) return SudoError.InvalidConvert;
    const t = @trunc(f);
    if (t < -9223372036854775808.0 or t >= 9223372036854775808.0)
        return SudoError.InvalidConvert;
    return @intFromFloat(t);
}

// ── SudoList: bounds-checked ArrayListUnmanaged wrapper ─────────────────────

/// Monomorphized list type. Element deep-copies are the caller's job at
/// insertion points; this wrapper only manages the contiguous buffer.
pub fn SudoList(comptime T: type) type {
    return struct {
        const Self = @This();
        alloc: std.mem.Allocator,
        list: std.ArrayListUnmanaged(T) = .empty,

        pub fn items(self: *const Self) []const T {
            return self.list.items;
        }

        pub fn itemsMut(self: *Self) []T {
            return self.list.items;
        }

        pub fn len(self: *const Self) i64 {
            return @intCast(self.list.items.len);
        }

        pub fn append(self: *Self, v: T) SudoError!void {
            self.list.append(self.alloc, v) catch return SudoError.InvalidArg;
        }

        pub fn at(self: *const Self, idx: i64) SudoError!T {
            if (idx < 0 or idx >= self.len()) return SudoError.OutOfBounds;
            return self.list.items[@intCast(idx)];
        }

        /// Mutable element pointer for `xs[i] = v` and `xs[i].f = v`.
        pub fn atPtr(self: *Self, idx: i64) SudoError!*T {
            if (idx < 0 or idx >= self.len()) return SudoError.OutOfBounds;
            return &self.list.items[@intCast(idx)];
        }

        pub fn put(self: *Self, idx: i64, v: T) SudoError!void {
            if (idx < 0 or idx >= self.len()) return SudoError.OutOfBounds;
            self.list.items[@intCast(idx)] = v;
        }

        pub fn pop(self: *Self) SudoError!T {
            if (self.list.items.len == 0) return SudoError.OutOfBounds;
            return self.list.orderedRemove(self.list.items.len - 1);
        }

        pub fn insert(self: *Self, idx: i64, v: T) SudoError!void {
            if (idx < 0 or idx > self.len()) return SudoError.OutOfBounds;
            self.list.insert(self.alloc, @intCast(idx), v) catch return SudoError.InvalidArg;
        }

        pub fn removeAt(self: *Self, idx: i64) SudoError!T {
            if (idx < 0 or idx >= self.len()) return SudoError.OutOfBounds;
            return self.list.orderedRemove(@intCast(idx));
        }

        pub fn swap(self: *Self, i: i64, j: i64) SudoError!void {
            const n = self.len();
            if (i < 0 or i >= n or j < 0 or j >= n) return SudoError.OutOfBounds;
            const a: usize = @intCast(i);
            const b: usize = @intCast(j);
            const tmp = self.list.items[a];
            self.list.items[a] = self.list.items[b];
            self.list.items[b] = tmp;
        }
    };
}

/// `rt.SudoResult(A, B)` is one type in every file.
pub fn SudoResult(comptime Ok: type, comptime Err: type) type {
    return union(enum) { Ok: Ok, Err: Err };
}

pub fn SudoTuple2(comptime T0: type, comptime T1: type) type {
    return struct { f0: T0, f1: T1 };
}
pub fn SudoTuple3(comptime T0: type, comptime T1: type, comptime T2: type) type {
    return struct { f0: T0, f1: T1, f2: T2 };
}
pub fn SudoTuple4(comptime T0: type, comptime T1: type, comptime T2: type, comptime T3: type) type {
    return struct { f0: T0, f1: T1, f2: T2, f3: T3 };
}
pub fn SudoTuple5(
    comptime T0: type,
    comptime T1: type,
    comptime T2: type,
    comptime T3: type,
    comptime T4: type,
) type {
    return struct { f0: T0, f1: T1, f2: T2, f3: T3, f4: T4 };
}
pub fn SudoTuple6(
    comptime T0: type,
    comptime T1: type,
    comptime T2: type,
    comptime T3: type,
    comptime T4: type,
    comptime T5: type,
) type {
    return struct { f0: T0, f1: T1, f2: T2, f3: T3, f4: T4, f5: T5 };
}
pub fn SudoTuple7(
    comptime T0: type,
    comptime T1: type,
    comptime T2: type,
    comptime T3: type,
    comptime T4: type,
    comptime T5: type,
    comptime T6: type,
) type {
    return struct { f0: T0, f1: T1, f2: T2, f3: T3, f4: T4, f5: T5, f6: T6 };
}
pub fn SudoTuple8(
    comptime T0: type,
    comptime T1: type,
    comptime T2: type,
    comptime T3: type,
    comptime T4: type,
    comptime T5: type,
    comptime T6: type,
    comptime T7: type,
) type {
    return struct { f0: T0, f1: T1, f2: T2, f3: T3, f4: T4, f5: T5, f6: T6, f7: T7 };
}

/// Insertion sort for `List<int>` — plain ascending `<` (stable).
pub fn sortI64(list: *SudoList(i64)) void {
    const items = list.list.items;
    var i: usize = 1;
    while (i < items.len) : (i += 1) {
        const key = items[i];
        var j: usize = i;
        while (j > 0 and items[j - 1] > key) : (j -= 1) {
            items[j] = items[j - 1];
        }
        items[j] = key;
    }
}

/// ascending; -0.0 < 0.0; NaN last.
fn f64SortLt(a: f64, b: f64) bool {
    if (std.math.isNan(a)) return false;
    if (std.math.isNan(b)) return true;
    if (a == b) return std.math.signbit(a) and !std.math.signbit(b);
    return a < b;
}

/// Stable insertion sort for `List<float>` (NaN last, -0.0 before 0.0).
pub fn sortF64(list: *SudoList(f64)) void {
    const items = list.list.items;
    var i: usize = 1;
    while (i < items.len) : (i += 1) {
        const key = items[i];
        var j: usize = i;
        while (j > 0 and f64SortLt(key, items[j - 1])) : (j -= 1) {
            items[j] = items[j - 1];
        }
        items[j] = key;
    }
}

// ── Structural key encoding for Map/Set ─────────────────────────────────────
//
// Map/Set keys are structural (a `List<int>` is a valid key). We encode each
// key to a canonical, injective byte string and store it in a
// `StringHashMapUnmanaged` alongside the original key. Stored keys are
// arena-duplicated so they outlive the shared scratch buffer.

var sudo_key_buf: [16384]u8 = undefined;
var sudo_key_len: usize = 0;

pub fn key_reset() void {
    sudo_key_len = 0;
}

pub fn key_bytes(s: []const u8) void {
    for (s) |c| {
        // Silent truncation would let two distinct keys collide (wrong Map/Set
        // semantics). Fail loud instead — the 16KB buffer is an impl limit.
        if (sudo_key_len >= sudo_key_buf.len) @panic("sudo: structural key exceeds 16KB encoding buffer");
        sudo_key_buf[sudo_key_len] = c;
        sudo_key_len += 1;
    }
}

pub fn key_i64(v: i64) void {
    var b: [24]u8 = undefined;
    const s = std.fmt.bufPrint(&b, "i{d};", .{v}) catch return;
    key_bytes(s);
}

pub fn key_bool(v: bool) void {
    key_bytes(if (v) "T;" else "F;");
}

pub fn key_list(comptime T: type, comptime ke: fn (T) void) fn (SudoList(T)) void {
    const S = struct {
        fn app(v: SudoList(T)) void {
            key_bytes("L[");
            for (v.items()) |x| ke(x);
            key_bytes("]");
        }
    };
    return S.app;
}

pub fn key_tuple2(
    comptime T0: type,
    comptime T1: type,
    comptime k0: fn (T0) void,
    comptime k1: fn (T1) void,
) fn (SudoTuple2(T0, T1)) void {
    const S = struct {
        fn app(v: SudoTuple2(T0, T1)) void {
            key_bytes("(");
            k0(v.f0);
            k1(v.f1);
            key_bytes(")");
        }
    };
    return S.app;
}

pub fn key_tuple3(
    comptime T0: type,
    comptime T1: type,
    comptime T2: type,
    comptime k0: fn (T0) void,
    comptime k1: fn (T1) void,
    comptime k2: fn (T2) void,
) fn (SudoTuple3(T0, T1, T2)) void {
    const S = struct {
        fn app(v: SudoTuple3(T0, T1, T2)) void {
            key_bytes("(");
            k0(v.f0);
            k1(v.f1);
            k2(v.f2);
            key_bytes(")");
        }
    };
    return S.app;
}

pub fn key_tuple4(
    comptime T0: type,
    comptime T1: type,
    comptime T2: type,
    comptime T3: type,
    comptime k0: fn (T0) void,
    comptime k1: fn (T1) void,
    comptime k2: fn (T2) void,
    comptime k3: fn (T3) void,
) fn (SudoTuple4(T0, T1, T2, T3)) void {
    const S = struct {
        fn app(v: SudoTuple4(T0, T1, T2, T3)) void {
            key_bytes("(");
            k0(v.f0);
            k1(v.f1);
            k2(v.f2);
            k3(v.f3);
            key_bytes(")");
        }
    };
    return S.app;
}

pub fn key_tuple5(
    comptime T0: type,
    comptime T1: type,
    comptime T2: type,
    comptime T3: type,
    comptime T4: type,
    comptime k0: fn (T0) void,
    comptime k1: fn (T1) void,
    comptime k2: fn (T2) void,
    comptime k3: fn (T3) void,
    comptime k4: fn (T4) void,
) fn (SudoTuple5(T0, T1, T2, T3, T4)) void {
    const S = struct {
        fn app(v: SudoTuple5(T0, T1, T2, T3, T4)) void {
            key_bytes("(");
            k0(v.f0);
            k1(v.f1);
            k2(v.f2);
            k3(v.f3);
            k4(v.f4);
            key_bytes(")");
        }
    };
    return S.app;
}

pub fn key_tuple6(
    comptime T0: type,
    comptime T1: type,
    comptime T2: type,
    comptime T3: type,
    comptime T4: type,
    comptime T5: type,
    comptime k0: fn (T0) void,
    comptime k1: fn (T1) void,
    comptime k2: fn (T2) void,
    comptime k3: fn (T3) void,
    comptime k4: fn (T4) void,
    comptime k5: fn (T5) void,
) fn (SudoTuple6(T0, T1, T2, T3, T4, T5)) void {
    const S = struct {
        fn app(v: SudoTuple6(T0, T1, T2, T3, T4, T5)) void {
            key_bytes("(");
            k0(v.f0);
            k1(v.f1);
            k2(v.f2);
            k3(v.f3);
            k4(v.f4);
            k5(v.f5);
            key_bytes(")");
        }
    };
    return S.app;
}

pub fn key_tuple7(
    comptime T0: type,
    comptime T1: type,
    comptime T2: type,
    comptime T3: type,
    comptime T4: type,
    comptime T5: type,
    comptime T6: type,
    comptime k0: fn (T0) void,
    comptime k1: fn (T1) void,
    comptime k2: fn (T2) void,
    comptime k3: fn (T3) void,
    comptime k4: fn (T4) void,
    comptime k5: fn (T5) void,
    comptime k6: fn (T6) void,
) fn (SudoTuple7(T0, T1, T2, T3, T4, T5, T6)) void {
    const S = struct {
        fn app(v: SudoTuple7(T0, T1, T2, T3, T4, T5, T6)) void {
            key_bytes("(");
            k0(v.f0);
            k1(v.f1);
            k2(v.f2);
            k3(v.f3);
            k4(v.f4);
            k5(v.f5);
            k6(v.f6);
            key_bytes(")");
        }
    };
    return S.app;
}

pub fn key_tuple8(
    comptime T0: type,
    comptime T1: type,
    comptime T2: type,
    comptime T3: type,
    comptime T4: type,
    comptime T5: type,
    comptime T6: type,
    comptime T7: type,
    comptime k0: fn (T0) void,
    comptime k1: fn (T1) void,
    comptime k2: fn (T2) void,
    comptime k3: fn (T3) void,
    comptime k4: fn (T4) void,
    comptime k5: fn (T5) void,
    comptime k6: fn (T6) void,
    comptime k7: fn (T7) void,
) fn (SudoTuple8(T0, T1, T2, T3, T4, T5, T6, T7)) void {
    const S = struct {
        fn app(v: SudoTuple8(T0, T1, T2, T3, T4, T5, T6, T7)) void {
            key_bytes("(");
            k0(v.f0);
            k1(v.f1);
            k2(v.f2);
            k3(v.f3);
            k4(v.f4);
            k5(v.f5);
            k6(v.f6);
            k7(v.f7);
            key_bytes(")");
        }
    };
    return S.app;
}

pub fn key_option(comptime T: type, comptime ke: fn (T) void) fn (?T) void {
    const S = struct {
        fn app(v: ?T) void {
            if (v) |p| {
                key_bytes("S(");
                ke(p);
                key_bytes(")");
            } else key_bytes("N;");
        }
    };
    return S.app;
}

pub fn key_option_box(comptime T: type, comptime ke: fn (T) void) fn (?*const T) void {
    const S = struct {
        fn app(v: ?*const T) void {
            if (v) |p| {
                key_bytes("S(");
                ke(p.*);
                key_bytes(")");
            } else key_bytes("N;");
        }
    };
    return S.app;
}

pub fn key_box(comptime T: type, comptime ke: fn (T) void) fn (*const T) void {
    const S = struct {
        fn app(p: *const T) void {
            ke(p.*);
        }
    };
    return S.app;
}

pub fn key_result(
    comptime Ok: type,
    comptime Err: type,
    comptime ko: fn (Ok) void,
    comptime ke: fn (Err) void,
) fn (SudoResult(Ok, Err)) void {
    const S = struct {
        fn app(v: SudoResult(Ok, Err)) void {
            switch (v) {
                .Ok => |p| {
                    key_bytes("K(");
                    ko(p);
                    key_bytes(")");
                },
                .Err => |p| {
                    key_bytes("X(");
                    ke(p);
                    key_bytes(")");
                },
            }
        }
    };
    return S.app;
}

pub fn key_slice() []const u8 {
    return sudo_key_buf[0..sudo_key_len];
}

/// Arena copy of the current scratch key (so it survives later encodings).
pub fn key_dup(alloc: std.mem.Allocator) []const u8 {
    return alloc.dupe(u8, key_slice()) catch @panic("sudo: arena OOM");
}

/// Structural map: string-encoded key → (original key, value). `appendKey`
/// writes the encoding of a key into the shared scratch buffer.
pub fn SudoMap(comptime K: type, comptime V: type, comptime appendKey: fn (K) void) type {
    return struct {
        const Self = @This();
        pub const KV = struct { k: K, v: V };
        alloc: std.mem.Allocator,
        map: std.StringHashMapUnmanaged(KV) = .empty,

        fn enc(k: K) []const u8 {
            key_reset();
            appendKey(k);
            return key_slice();
        }

        pub fn put(self: *Self, k: K, v: V) void {
            const e = enc(k);
            const gop = self.map.getOrPut(self.alloc, e) catch @panic("sudo: arena OOM");
            if (!gop.found_existing) {
                gop.key_ptr.* = self.alloc.dupe(u8, e) catch @panic("sudo: arena OOM");
            }
            gop.value_ptr.* = .{ .k = k, .v = v };
        }

        pub fn getPtr(self: *const Self, k: K) ?*V {
            if (self.map.getPtr(enc(k))) |kv| return &kv.v;
            return null;
        }

        pub fn index(self: *const Self, k: K) SudoError!V {
            if (self.map.getPtr(enc(k))) |kv| return kv.v;
            return SudoError.KeyMissing;
        }

        pub fn has(self: *const Self, k: K) bool {
            return self.map.contains(enc(k));
        }

        pub fn delete(self: *Self, k: K) bool {
            return self.map.remove(enc(k));
        }

        pub fn size(self: *const Self) i64 {
            return @intCast(self.map.count());
        }
    };
}

/// Structural set: string-encoded element → original element.
pub fn SudoSet(comptime E: type, comptime appendKey: fn (E) void) type {
    return struct {
        const Self = @This();
        alloc: std.mem.Allocator,
        map: std.StringHashMapUnmanaged(E) = .empty,

        fn enc(e: E) []const u8 {
            key_reset();
            appendKey(e);
            return key_slice();
        }

        /// Returns true if newly inserted.
        pub fn add(self: *Self, e: E) bool {
            const s = enc(e);
            const gop = self.map.getOrPut(self.alloc, s) catch @panic("sudo: arena OOM");
            if (!gop.found_existing) {
                gop.key_ptr.* = self.alloc.dupe(u8, s) catch @panic("sudo: arena OOM");
                gop.value_ptr.* = e;
                return true;
            }
            return false;
        }

        pub fn has(self: *const Self, e: E) bool {
            return self.map.contains(enc(e));
        }

        pub fn remove(self: *Self, e: E) bool {
            return self.map.remove(enc(e));
        }

        pub fn size(self: *const Self) i64 {
            return @intCast(self.map.count());
        }
    };
}

// ── Assert-failure detail buffer ────────────────────────────────────────────

pub var sudo_trap_detail: [4096]u8 = undefined;
var sudo_det_len: usize = 0;

pub fn det_reset() void {
    sudo_det_len = 0;
    sudo_trap_detail[0] = 0;
}

pub fn det_str(s: []const u8) void {
    for (s) |c| {
        if (sudo_det_len + 1 >= sudo_trap_detail.len) break;
        sudo_trap_detail[sudo_det_len] = c;
        sudo_det_len += 1;
    }
    sudo_trap_detail[sudo_det_len] = 0;
}

pub fn det_i64(v: i64) void {
    var buf: [32]u8 = undefined;
    const s = std.fmt.bufPrint(&buf, "{d}", .{v}) catch return;
    det_str(s);
}

/// JS-canon-compatible float formatting for assert diagnostics.
pub fn det_f64(v: f64) void {
    if (std.math.isNan(v)) {
        det_str("NaN");
        return;
    }
    if (std.math.isInf(v)) {
        det_str(if (v > 0) "Inf" else "-Inf");
        return;
    }
    if (v == 0.0 and std.math.signbit(v)) {
        det_str("-0.0");
        return;
    }
    var buf: [64]u8 = undefined;
    const s = std.fmt.bufPrint(&buf, "{d}", .{v}) catch return;
    var integral = s.len > 0;
    for (s) |c| {
        if (c != '-' and (c < '0' or c > '9')) {
            integral = false;
            break;
        }
    }
    det_str(s);
    if (integral) det_str(".0");
}

pub fn det_bool(v: bool) void {
    det_str(if (v) "true" else "false");
}

pub fn det_line_prefix(line: u32) void {
    det_str("line ");
    var buf: [16]u8 = undefined;
    const s = std.fmt.bufPrint(&buf, "{d}", .{line}) catch return;
    det_str(s);
    det_str(": ");
}

pub fn assertEqI64(lhs: i64, rhs: i64, line: u32) SudoError!void {
    if (lhs == rhs) return;
    det_reset();
    det_line_prefix(line);
    det_i64(lhs);
    det_str(" != ");
    det_i64(rhs);
    return SudoError.AssertFailed;
}

pub fn assertEqF64(lhs: f64, rhs: f64, line: u32) SudoError!void {
    // sudo float equality is IEEE (`NaN != NaN`); the pass path uses `==`.
    if (lhs == rhs) return;
    det_reset();
    det_line_prefix(line);
    det_f64(lhs);
    det_str(" != ");
    det_f64(rhs);
    return SudoError.AssertFailed;
}

pub fn assertEqBool(lhs: bool, rhs: bool, line: u32) SudoError!void {
    if (lhs == rhs) return;
    det_reset();
    det_line_prefix(line);
    det_bool(lhs);
    det_str(" != ");
    det_bool(rhs);
    return SudoError.AssertFailed;
}

/// `expect_trap` fell through without trapping.
pub fn expectTrapNone(line: u32, kind: []const u8) SudoError {
    det_reset();
    det_line_prefix(line);
    det_str("expected trap ");
    det_str(kind);
    det_str(", but nothing trapped");
    return SudoError.AssertFailed;
}

/// `expect_trap` observed the wrong trap kind.
pub fn expectTrapWrong(line: u32, kind: []const u8, got: []const u8) SudoError {
    det_reset();
    det_line_prefix(line);
    det_str("expected trap ");
    det_str(kind);
    det_str(", got ");
    det_str(got);
    return SudoError.AssertFailed;
}

// ── TAP runner ──────────────────────────────────────────────────────────────

pub const TestCase = struct { name: []const u8, func: *const fn () SudoError!void };

fn writeStdout(s: []const u8) void {
    _ = std.c.write(1, s.ptr, s.len);
}

fn printOk(n: usize, name: []const u8) void {
    var buf: [512]u8 = undefined;
    const s = std.fmt.bufPrint(&buf, "ok {d} - {s}\n", .{ n, name }) catch return;
    writeStdout(s);
}

/// `detail` empty → `not ok N - name [Kind]`; else `not ok N - name [Kind: detail]`.
fn printNotOk(n: usize, name: []const u8, kind: []const u8, detail: []const u8) void {
    var buf: [4608]u8 = undefined;
    const s = if (detail.len == 0)
        std.fmt.bufPrint(&buf, "not ok {d} - {s} [{s}]\n", .{ n, name, kind }) catch return
    else
        std.fmt.bufPrint(&buf, "not ok {d} - {s} [{s}: {s}]\n", .{ n, name, kind, detail }) catch return;
    writeStdout(s);
}

fn detailSlice() []const u8 {
    return sudo_trap_detail[0..sudo_det_len];
}

/// Run every test in declaration order, printing the outcome protocol.
/// Each test owns its own arena (freed by `defer` inside the test fn).
/// Exits nonzero on any failure or Debug-mode leak (never returns).
pub fn run_tests(tests: []const TestCase) void {
    var failures: usize = 0;
    for (tests, 1..) |tc, n| {
        det_reset();
        if (tc.func()) |_| {
            printOk(n, tc.name);
        } else |err| {
            failures += 1;
            printNotOk(n, tc.name, @errorName(err), detailSlice());
        }
    }
    const leaked = backingReport();
    if (leaked != 0) {
        writeStdout("sudo: DebugAllocator reported leak(s)\n");
    }
    std.process.exit(if (failures == 0 and leaked == 0) 0 else 1);
}
