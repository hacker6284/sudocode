//! sudo runtime for the Zig backend (Zig 0.16).
//!
//! Trap surface: a closed error set; sudo functions return `SudoError!T` and
//! propagate with `try`; `expect_trap` observes an error locally. Trap kinds
//! map 1:1 onto sudo's spec §8 set, so `@errorName` is the reported kind.
//!
//! Memory (v1): ONE global arena over the page allocator. Every sudo heap
//! value (list buffers, boxed enum/option payloads, map keys/entries) is
//! allocated from it. The test runner resets the arena between tests
//! (`.reset(.retain_capacity)`), so a trap that abandons in-flight values
//! leaks nothing across tests — there are no per-value frees. See
//! notes/friction-zig.md for the rationale and the v2 upgrade path.

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

/// Global arena for all sudo heap values; reset between tests by `run_tests`.
pub var sudo_arena: std.heap.ArenaAllocator = std.heap.ArenaAllocator.init(std.heap.page_allocator);

pub fn allocator() std.mem.Allocator {
    return sudo_arena.allocator();
}

/// Arena-allocate a single value (used when boxing enum/Option/Result payloads).
pub fn box(comptime T: type, v: T) *const T {
    const p = allocator().create(T) catch @panic("sudo: arena OOM");
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
            self.list.append(allocator(), v) catch return SudoError.InvalidArg;
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
            self.list.insert(allocator(), @intCast(idx), v) catch return SudoError.InvalidArg;
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
        if (sudo_key_len >= sudo_key_buf.len) break;
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

pub fn key_slice() []const u8 {
    return sudo_key_buf[0..sudo_key_len];
}

/// Arena copy of the current scratch key (so it survives later encodings).
pub fn key_dup() []const u8 {
    return allocator().dupe(u8, key_slice()) catch @panic("sudo: arena OOM");
}

/// Structural map: string-encoded key → (original key, value). `appendKey`
/// writes the encoding of a key into the shared scratch buffer.
pub fn SudoMap(comptime K: type, comptime V: type, comptime appendKey: fn (K) void) type {
    return struct {
        const Self = @This();
        pub const KV = struct { k: K, v: V };
        map: std.StringHashMapUnmanaged(KV) = .empty,

        fn enc(k: K) []const u8 {
            key_reset();
            appendKey(k);
            return key_slice();
        }

        pub fn put(self: *Self, k: K, v: V) void {
            const e = enc(k);
            const gop = self.map.getOrPut(allocator(), e) catch @panic("sudo: arena OOM");
            if (!gop.found_existing) {
                gop.key_ptr.* = allocator().dupe(u8, e) catch @panic("sudo: arena OOM");
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
        map: std.StringHashMapUnmanaged(E) = .empty,

        fn enc(e: E) []const u8 {
            key_reset();
            appendKey(e);
            return key_slice();
        }

        /// Returns true if newly inserted.
        pub fn add(self: *Self, e: E) bool {
            const s = enc(e);
            const gop = self.map.getOrPut(allocator(), s) catch @panic("sudo: arena OOM");
            if (!gop.found_existing) {
                gop.key_ptr.* = allocator().dupe(u8, s) catch @panic("sudo: arena OOM");
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
/// Resets the arena and detail buffer between tests. Exits nonzero on any
/// failure (never returns).
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
        _ = sudo_arena.reset(.retain_capacity);
    }
    std.process.exit(if (failures == 0) 0 else 1);
}
