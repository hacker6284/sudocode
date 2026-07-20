// The sudo Swift runtime. Shipped alongside generated modules by sudoc.
// Implements the semantics pinned in spec/language.md: Int64 with explicit
// Overflow traps, floor division, IEEE float edges, Option/Result wrappers
// that do not flatten, SudoRange for continue-safe for-range loops, and TAP
// test runner.

import Foundation

// ---- traps -----------------------------------------------------------------

struct SudoTrap: Error {
    let kind: String
    let detail: String
}

// ---- checked Int64 arithmetic (only place bare + - * / % may appear) -------

func chkAdd(_ a: Int64, _ b: Int64) throws -> Int64 {
    let (r, o) = a.addingReportingOverflow(b)
    if o { throw SudoTrap(kind: "Overflow", detail: "") }
    return r
}

func chkSub(_ a: Int64, _ b: Int64) throws -> Int64 {
    let (r, o) = a.subtractingReportingOverflow(b)
    if o { throw SudoTrap(kind: "Overflow", detail: "") }
    return r
}

func chkMul(_ a: Int64, _ b: Int64) throws -> Int64 {
    let (r, o) = a.multipliedReportingOverflow(by: b)
    if o { throw SudoTrap(kind: "Overflow", detail: "") }
    return r
}

func chkNeg(_ a: Int64) throws -> Int64 {
    if a == Int64.min { throw SudoTrap(kind: "Overflow", detail: "") }
    return -a
}

func chkAbs(_ a: Int64) throws -> Int64 {
    a < 0 ? try chkNeg(a) : a
}

/// Floor division (toward −∞); traps DivByZero and MIN/−1 Overflow.
func floorDiv(_ a: Int64, _ b: Int64) throws -> Int64 {
    if b == 0 { throw SudoTrap(kind: "DivByZero", detail: "") }
    if b == -1 { return try chkNeg(a) }
    let q = a / b
    let r = a % b
    return (r != 0 && (r < 0) != (b < 0)) ? q - 1 : q
}

/// Floor modulo (sign of divisor).
func floorMod(_ a: Int64, _ b: Int64) throws -> Int64 {
    if b == 0 { throw SudoTrap(kind: "DivByZero", detail: "") }
    if b == -1 { return 0 }
    let r = a % b
    return (r != 0 && (r < 0) != (b < 0)) ? r + b : r
}

func minInt(_ a: Int64, _ b: Int64) -> Int64 { a < b ? a : b }
func maxInt(_ a: Int64, _ b: Int64) -> Int64 { a > b ? a : b }

// ---- IEEE float helpers (spec §4.3) ----------------------------------------

/// NaN if either operand is NaN; min(−0.0, 0.0) == −0.0 (unlike Darwin fmin).
func sudoFmin(_ a: Double, _ b: Double) -> Double {
    if a.isNaN || b.isNaN { return Double.nan }
    if a == b { return a.sign == .minus ? a : b }
    return a < b ? a : b
}

func sudoFmax(_ a: Double, _ b: Double) -> Double {
    if a.isNaN || b.isNaN { return Double.nan }
    if a == b { return a.sign == .minus ? b : a }
    return a > b ? a : b
}

func sudoFloor(_ x: Double) -> Double { x.rounded(.down) }
func sudoCeil(_ x: Double) -> Double { x.rounded(.up) }

/// Ties away from zero (spec §4.3).
func sudoRound(_ x: Double) -> Double {
    x.rounded(.toNearestOrAwayFromZero)
}

func sudoSqrt(_ x: Double) -> Double {
    if x.isNaN || x < 0.0 { return Double.nan }
    return x.squareRoot()
}

func intOfFloat(_ f: Double) throws -> Int64 {
    if f.isNaN || !f.isFinite {
        throw SudoTrap(kind: "InvalidConvert", detail: "")
    }
    let t = f.rounded(.towardZero)
    // Compare in Double space against the exclusive upper bound (mirror sudo_int_of).
    if t < -9223372036854775808.0 || t >= 9223372036854775808.0 {
        throw SudoTrap(kind: "InvalidConvert", detail: "")
    }
    return Int64(t)
}

/// Sort order for List<float>.sort(): NaN last, −0.0 before 0.0.
func sudoF64SortLt(_ a: Double, _ b: Double) -> Bool {
    if a.isNaN { return false }
    if b.isNaN { return true }
    if a == b { return a.sign == .minus && b.sign != .minus }
    return a < b
}

// ---- Option / Result (do NOT use Swift Optional — nested Option must stay distinct)

enum SudoOption<T> {
    indirect case some(T)
    case none
}

extension SudoOption: Equatable where T: Equatable {}
extension SudoOption: Hashable where T: Hashable {}

enum SudoResult<T, E> {
    indirect case ok(T)
    indirect case err(E)
}

extension SudoResult: Equatable where T: Equatable, E: Equatable {}
extension SudoResult: Hashable where T: Hashable, E: Hashable {}

func optIsSome<T>(_ o: SudoOption<T>) -> Bool {
    if case .some = o { return true }
    return false
}

func optIsNone<T>(_ o: SudoOption<T>) -> Bool { !optIsSome(o) }

func optUnwrap<T>(_ o: SudoOption<T>) throws -> T {
    if case .some(let v) = o { return v }
    throw SudoTrap(kind: "UnwrapFailed", detail: "")
}

func optGetOr<T>(_ o: SudoOption<T>, _ d: T) -> T {
    if case .some(let v) = o { return v }
    return d
}

func resIsOk<T, E>(_ r: SudoResult<T, E>) -> Bool {
    if case .ok = r { return true }
    return false
}

func resIsErr<T, E>(_ r: SudoResult<T, E>) -> Bool { !resIsOk(r) }

func resUnwrap<T, E>(_ r: SudoResult<T, E>) throws -> T {
    if case .ok(let v) = r { return v }
    throw SudoTrap(kind: "UnwrapFailed", detail: "")
}

func resGetOr<T, E>(_ r: SudoResult<T, E>, _ d: T) -> T {
    if case .ok(let v) = r { return v }
    return d
}

// ---- list / map helpers ----------------------------------------------------

func listAt<T>(_ a: [T], _ i: Int64) throws -> T {
    if i < 0 || i >= Int64(a.count) {
        throw SudoTrap(kind: "OutOfBounds", detail: "")
    }
    return a[Int(i)]
}

func listSet<T>(_ a: inout [T], _ i: Int64, _ v: T) throws {
    if i < 0 || i >= Int64(a.count) {
        throw SudoTrap(kind: "OutOfBounds", detail: "")
    }
    a[Int(i)] = v
}

func listPop<T>(_ a: inout [T]) throws -> T {
    if a.isEmpty { throw SudoTrap(kind: "OutOfBounds", detail: "") }
    return a.removeLast()
}

func listInsert<T>(_ a: inout [T], _ i: Int64, _ v: T) throws {
    if i < 0 || i > Int64(a.count) {
        throw SudoTrap(kind: "OutOfBounds", detail: "")
    }
    a.insert(v, at: Int(i))
}

func listRemoveAt<T>(_ a: inout [T], _ i: Int64) throws -> T {
    if i < 0 || i >= Int64(a.count) {
        throw SudoTrap(kind: "OutOfBounds", detail: "")
    }
    return a.remove(at: Int(i))
}

func listSwap<T>(_ a: inout [T], _ i: Int64, _ j: Int64) throws {
    if i < 0 || i >= Int64(a.count) || j < 0 || j >= Int64(a.count) {
        throw SudoTrap(kind: "OutOfBounds", detail: "")
    }
    a.swapAt(Int(i), Int(j))
}

func listSortInt(_ a: inout [Int64]) {
    a.sort()
}

func listSortFloat(_ a: inout [Double]) {
    a.sort { sudoF64SortLt($0, $1) }
}

func filled<T>(_ n: Int64, _ v: T) throws -> [T] {
    if n < 0 { throw SudoTrap(kind: "InvalidArg", detail: "") }
    return Array(repeating: v, count: Int(n))
}

func mapAt<K: Hashable, V>(_ m: [K: V], _ k: K) throws -> V {
    guard let v = m[k] else {
        throw SudoTrap(kind: "KeyMissing", detail: "")
    }
    return v
}

func mapGetOpt<K: Hashable, V>(_ m: [K: V], _ k: K) -> SudoOption<V> {
    if let v = m[k] { return .some(v) }
    return .none
}

// ---- inclusive Int64 range (continue-safe; safe at Int64.max/min) ----------

struct SudoRange: Sequence, IteratorProtocol {
    var current: Int64
    let bound: Int64
    let down: Bool
    var done: Bool

    /// `lo`/`hi` are the evaluated `from`/`to` bounds (inclusive).
    /// Ascending: from lo to hi; descending: from lo downto hi.
    init(_ lo: Int64, _ hi: Int64, down: Bool) {
        current = lo
        bound = hi
        self.down = down
        done = down ? (lo < hi) : (lo > hi)
    }

    mutating func next() -> Int64? {
        if done { return nil }
        let v = current
        done = (current == bound)
        if !done {
            // Wrap only on the terminal step path; wrapped value is never yielded.
            current = down ? current &- 1 : current &+ 1
        }
        return v
    }
}

func sudoRange(_ lo: Int64, _ hi: Int64, down: Bool) -> SudoRange {
    SudoRange(lo, hi, down: down)
}

// ---- assert / canon / TAP runner -------------------------------------------

func sudoAssert(_ cond: Bool, _ line: Int) throws {
    if !cond {
        throw SudoTrap(kind: "AssertFailed", detail: "line \(line)")
    }
}

func sudoAssertEq<T: Equatable>(_ l: T, _ r: T, _ line: Int) throws {
    if l != r {
        throw SudoTrap(
            kind: "AssertFailed",
            detail: "line \(line): \(canon(l)) != \(canon(r))"
        )
    }
}

/// Diagnostic-only canonical form (lockstep compares trap kinds, not detail).
func canon(_ v: Any) -> String {
    if let b = v as? Bool {
        return b ? "true" : "false"
    }
    if let i = v as? Int64 {
        return String(i)
    }
    if let i = v as? Int {
        return String(i)
    }
    if let d = v as? Double {
        if d.isNaN { return #"{"f": "NaN"}"# }
        if d == Double.infinity { return #"{"f": "Inf"}"# }
        if d == -Double.infinity { return #"{"f": "-Inf"}"# }
        if d == 0.0 && d.sign == .minus { return #"{"f": "-0.0"}"# }
        if d.rounded(.towardZero) == d && d.isFinite {
            return String(format: "%.1f", d)
        }
        // Shortest round-trip-ish; diagnostic only.
        var s = String(d)
        if !s.contains(".") && !s.contains("e") && !s.contains("E") {
            s += ".0"
        }
        return s
    }
    if let arr = v as? [Any] {
        return "[" + arr.map { canon($0) }.joined(separator: ", ") + "]"
    }
    // Generic Mirror walk for arrays of concrete T, dictionaries, sets, structs, enums.
    let m = Mirror(reflecting: v)
    switch m.displayStyle {
    case .some(.collection):
        let parts = m.children.map { canon($0.value) }
        return "[" + parts.joined(separator: ", ") + "]"
    case .some(.dictionary):
        // Order-insensitive for diagnostics: sort by rendered key.
        var pairs: [(String, String)] = []
        for child in m.children {
            let cm = Mirror(reflecting: child.value)
            var it = cm.children.makeIterator()
            if let k = it.next(), let val = it.next() {
                pairs.append((canon(k.value), canon(val.value)))
            }
        }
        pairs.sort { $0.0 < $1.0 }
        let body = pairs.map { "\($0.0): \($0.1)" }.joined(separator: ", ")
        return "{" + body + "}"
    case .some(.set):
        var items = m.children.map { canon($0.value) }
        items.sort()
        return "{" + items.joined(separator: ", ") + "}"
    case .some(.struct):
        let fields = m.children.map { child -> String in
            if let label = child.label {
                return "\(label): \(canon(child.value))"
            }
            return canon(child.value)
        }
        let name = String(describing: type(of: v)).split(separator: "<").first.map(String.init) ?? "struct"
        return "\(name)(\(fields.joined(separator: ", ")))"
    case .some(.enum):
        if let child = m.children.first {
            let label = child.label ?? "case"
            let payload = Mirror(reflecting: child.value)
            if payload.children.isEmpty {
                // Associated value may itself be a tuple of fields.
                let inner = String(describing: child.value)
                if inner == "()" || inner.isEmpty {
                    return label
                }
                // Prefer walking associated-value tuple.
                let parts = payload.children.map { canon($0.value) }
                if parts.isEmpty {
                    return "\(label)(\(canon(child.value)))"
                }
                return "\(label)(\(parts.joined(separator: ", ")))"
            }
            let parts = payload.children.map { canon($0.value) }
            return "\(label)(\(parts.joined(separator: ", ")))"
        }
        return String(describing: v)
    case .some(.tuple):
        let parts = m.children.map { canon($0.value) }
        return "(" + parts.joined(separator: ", ") + ")"
    default:
        return String(describing: v)
    }
}

func runTests(_ tests: [(String, () throws -> Void)]) -> Int32 {
    var failures = 0
    for (i, (name, fn)) in tests.enumerated() {
        do {
            try fn()
            print("ok \(i + 1) - \(name)")
        } catch let t as SudoTrap {
            failures += 1
            let d = t.detail.isEmpty ? "" : ": \(t.detail)"
            print("not ok \(i + 1) - \(name) [\(t.kind)\(d)]")
        } catch {
            failures += 1
            print("not ok \(i + 1) - \(name) [Unknown: \(error)]")
        }
    }
    print("# \(tests.count - failures)/\(tests.count) passed")
    return failures > 0 ? 1 : 0
}
