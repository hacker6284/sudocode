//! The sudo Rust runtime. Shipped alongside generated modules by sudoc.
//! Implements semantics from spec/language.md: checked i64 arithmetic, floor
//! div/mod, IEEE float edges, bounds-checked list ops, trap raise/observe,
//! canonical serialization for assert diagnostics, and the TAP test runner.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::hash::Hash;
use std::panic::{self, AssertUnwindSafe};

/// A defined runtime fault (spec §8). Kind is one of the closed set.
#[derive(Debug, Clone)]
pub struct SudoTrap {
    pub kind: &'static str,
    pub detail: String,
}

impl SudoTrap {
    pub fn new(kind: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

#[inline]
pub fn trap(kind: &'static str, detail: impl Into<String>) -> ! {
    panic::panic_any(SudoTrap::new(kind, detail));
}

#[inline]
pub fn trap_kind(kind: &'static str) -> ! {
    trap(kind, "");
}

// ---- i64 arithmetic --------------------------------------------------------

#[inline]
pub fn chk_add(a: i64, b: i64) -> i64 {
    a.checked_add(b).unwrap_or_else(|| trap_kind("Overflow"))
}

#[inline]
pub fn chk_sub(a: i64, b: i64) -> i64 {
    a.checked_sub(b).unwrap_or_else(|| trap_kind("Overflow"))
}

#[inline]
pub fn chk_mul(a: i64, b: i64) -> i64 {
    a.checked_mul(b).unwrap_or_else(|| trap_kind("Overflow"))
}

/// Floor division (toward -∞); traps DivByZero and MIN/-1 Overflow.
#[inline]
pub fn div(a: i64, b: i64) -> i64 {
    if b == 0 {
        trap_kind("DivByZero");
    }
    if a == i64::MIN && b == -1 {
        trap_kind("Overflow");
    }
    // Rust `/` truncates toward zero; convert to floor.
    let q = a / b;
    let r = a % b;
    if r != 0 && (a < 0) != (b < 0) {
        q - 1
    } else {
        q
    }
}

/// Floor modulo (sign of divisor). `b == -1` is special-cased:
/// mathematically the result is always 0 (it fits; only the
/// equivalent division overflows for `a == i64::MIN`), but Rust's `%`
/// panics on `i64::MIN % -1` (checked-overflow, same trigger as `/`),
/// so compute it directly instead of going through `%`.
#[inline]
pub fn mod_i64(a: i64, b: i64) -> i64 {
    if b == 0 {
        trap_kind("DivByZero");
    }
    if b == -1 {
        return 0;
    }
    let r = a % b;
    if r != 0 && (a < 0) != (b < 0) {
        r + b
    } else {
        r
    }
}

#[inline]
pub fn abs_i64(x: i64) -> i64 {
    if x == i64::MIN {
        trap_kind("Overflow");
    }
    if x < 0 {
        -x
    } else {
        x
    }
}

#[inline]
pub fn neg(x: i64) -> i64 {
    x.checked_neg().unwrap_or_else(|| trap_kind("Overflow"))
}

// ---- floats ----------------------------------------------------------------

#[inline]
pub fn fdiv(a: f64, b: f64) -> f64 {
    if b == 0.0 {
        if a == 0.0 || a.is_nan() {
            return f64::NAN;
        }
        let sa = if a.is_sign_negative() { -1.0 } else { 1.0 };
        let sb = if b.is_sign_negative() { -1.0 } else { 1.0 };
        return sa * sb * f64::INFINITY;
    }
    a / b
}

#[inline]
pub fn fmin(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a == b {
        // min(-0.0, 0.0) == -0.0
        if a.is_sign_negative() {
            return a;
        }
        if b.is_sign_negative() {
            return b;
        }
        return a;
    }
    if a < b {
        a
    } else {
        b
    }
}

#[inline]
pub fn fmax(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a == b {
        // max(-0.0, 0.0) == +0.0
        if a.is_sign_negative() {
            return b;
        }
        return a;
    }
    if a > b {
        a
    } else {
        b
    }
}

#[inline]
pub fn floor(x: f64) -> f64 {
    if x.is_nan() || x.is_infinite() {
        return x;
    }
    x.floor()
}

#[inline]
pub fn ceil(x: f64) -> f64 {
    if x.is_nan() || x.is_infinite() {
        return x;
    }
    x.ceil()
}

/// Ties away from zero (spec §4.3), not bankers' rounding. `f64::round`
/// is already IEEE round-half-away-from-zero natively (verified
/// against a C oracle: half-ulp-below-tie, exact ties, NaN/Inf
/// passthrough, and signed-zero cases all match), so this is a thin
/// wrapper rather than a hand-rolled floor/ceil-of-(x±0.5)
/// implementation, which double-rounds values just below a half
/// boundary (e.g. 0.49999999999999994) up past the tie.
#[inline]
pub fn round_half_away(x: f64) -> f64 {
    x.round()
}

#[inline]
pub fn sqrt(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    x.sqrt()
}

#[inline]
pub fn int_of(x: f64) -> i64 {
    if x.is_nan() || x.is_infinite() {
        trap("InvalidConvert", "NaN or infinity to int");
    }
    // Truncate toward zero, then range-check. Bounds exact in f64.
    if x >= 9223372036854775808.0 || x < -9223372036854775808.0 {
        trap("InvalidConvert", "float out of int range");
    }
    x as i64
}

// ---- lists -----------------------------------------------------------------

#[inline]
fn idx(len: usize, i: i64) -> usize {
    if i < 0 || (i as u64) >= (len as u64) {
        trap("OutOfBounds", format!("index {i} of length {len}"));
    }
    i as usize
}

#[inline]
pub fn at<T: Clone>(a: &[T], i: i64) -> T {
    a[idx(a.len(), i)].clone()
}

#[inline]
pub fn at_mut<T>(a: &mut [T], i: i64) -> &mut T {
    let n = a.len();
    &mut a[idx(n, i)]
}

#[inline]
pub fn put<T>(a: &mut [T], i: i64, v: T) {
    a[idx(a.len(), i)] = v;
}

#[inline]
pub fn pop<T>(a: &mut Vec<T>) -> T {
    a.pop()
        .unwrap_or_else(|| trap("OutOfBounds", "pop from empty list"))
}

#[inline]
pub fn insert<T>(a: &mut Vec<T>, i: i64, v: T) {
    let n = a.len();
    if i < 0 || (i as u64) > (n as u64) {
        trap("OutOfBounds", format!("insert at {i} of length {n}"));
    }
    a.insert(i as usize, v);
}

#[inline]
pub fn remove_at<T>(a: &mut Vec<T>, i: i64) -> T {
    let j = idx(a.len(), i);
    a.remove(j)
}

#[inline]
pub fn swap<T>(a: &mut [T], i: i64, j: i64) {
    let n = a.len();
    let ii = idx(n, i);
    let jj = idx(n, j);
    a.swap(ii, jj);
}

/// Stable float sort: NaN last, -0.0 before +0.0 (spec §7). Not f64::total_cmp.
pub fn sort_floats(a: &mut [f64]) {
    a.sort_by(|x, y| {
        let kx = float_sort_key(*x);
        let ky = float_sort_key(*y);
        if kx.0 != ky.0 {
            return kx.0.cmp(&ky.0);
        }
        if kx.0 == 2 {
            return std::cmp::Ordering::Equal;
        }
        if kx.1 < ky.1 {
            return std::cmp::Ordering::Less;
        }
        if kx.1 > ky.1 {
            return std::cmp::Ordering::Greater;
        }
        kx.2.cmp(&ky.2)
    });
}

/// (nan_group, value, sign) — nan_group 2 last; among reals ordinary <; ±0 by sign.
fn float_sort_key(x: f64) -> (u8, f64, i8) {
    if x.is_nan() {
        return (2, 0.0, 0);
    }
    let sign: i8 = if x == 0.0 {
        if x.is_sign_negative() {
            -1
        } else {
            1
        }
    } else if x < 0.0 {
        -1
    } else {
        1
    };
    (1, x, sign)
}

#[inline]
pub fn filled<T: Clone>(n: i64, v: &T) -> Vec<T> {
    if n < 0 {
        trap("InvalidArg", format!("filled({n})"));
    }
    let count = n as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(v.clone());
    }
    out
}

#[inline]
pub fn list_concat<T: Clone>(a: &[T], b: &[T]) -> Vec<T> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    out.extend_from_slice(a);
    out.extend_from_slice(b);
    out
}

/// Text literal helper: Unicode scalar values → `Vec<i64>`.
#[inline]
pub fn text(scalars: &[i64]) -> Vec<i64> {
    scalars.to_vec()
}

// ---- maps ------------------------------------------------------------------

#[inline]
pub fn map_get<K: Eq + Hash, V: Clone>(m: &HashMap<K, V>, k: &K) -> V {
    match m.get(k) {
        Some(v) => v.clone(),
        None => trap_kind("KeyMissing"),
    }
}

#[inline]
pub fn map_get_mut<'a, K: Eq + Hash, V>(m: &'a mut HashMap<K, V>, k: &K) -> &'a mut V {
    match m.get_mut(k) {
        Some(v) => v,
        None => trap_kind("KeyMissing"),
    }
}

// ---- Option / Result helpers -----------------------------------------------

#[inline]
pub fn unwrap_opt<T>(o: Option<T>) -> T {
    o.unwrap_or_else(|| trap_kind("UnwrapFailed"))
}

#[inline]
pub fn unwrap_res<T, E>(r: Result<T, E>) -> T {
    r.unwrap_or_else(|_| trap_kind("UnwrapFailed"))
}

// ---- canon / assert --------------------------------------------------------

/// Trait for diagnostic canonical serialization (lockstep.md §4, v1 diagnostic).
pub trait SudoCanon {
    fn canon(&self) -> String;
}

impl SudoCanon for i64 {
    fn canon(&self) -> String {
        self.to_string()
    }
}

impl SudoCanon for bool {
    fn canon(&self) -> String {
        if *self {
            "true".into()
        } else {
            "false".into()
        }
    }
}

impl SudoCanon for f64 {
    fn canon(&self) -> String {
        let s = if self.is_nan() {
            "NaN".into()
        } else if self.is_infinite() {
            if self.is_sign_positive() {
                "Inf".into()
            } else {
                "-Inf".into()
            }
        } else if *self == 0.0 && self.is_sign_negative() {
            "-0.0".into()
        } else {
            let mut s = format!("{self}");
            if !s.contains('.') && !s.contains('e') && !s.contains('E') {
                s.push_str(".0");
            }
            s
        };
        format!("{{\"f\": \"{s}\"}}")
    }
}

impl<T: SudoCanon> SudoCanon for Vec<T> {
    fn canon(&self) -> String {
        let mut out = String::from("[");
        for (i, x) in self.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&x.canon());
        }
        out.push(']');
        out
    }
}

impl<T: SudoCanon> SudoCanon for Box<T> {
    fn canon(&self) -> String {
        (**self).canon()
    }
}

impl<T: SudoCanon> SudoCanon for Option<T> {
    fn canon(&self) -> String {
        match self {
            Some(v) => format!("{{\"e\": \"Option.Some\", \"v\": [{}]}}", v.canon()),
            None => "{\"e\": \"Option.None\"}".into(),
        }
    }
}

impl<T: SudoCanon, E: SudoCanon> SudoCanon for Result<T, E> {
    fn canon(&self) -> String {
        match self {
            Ok(v) => format!("{{\"e\": \"Result.Ok\", \"v\": [{}]}}", v.canon()),
            Err(e) => format!("{{\"e\": \"Result.Err\", \"v\": [{}]}}", e.canon()),
        }
    }
}

impl<K: SudoCanon, V: SudoCanon> SudoCanon for HashMap<K, V> {
    fn canon(&self) -> String {
        let mut pairs = String::new();
        for (i, (k, v)) in self.iter().enumerate() {
            if i > 0 {
                pairs.push_str(", ");
            }
            let _ = write!(pairs, "[{}, {}]", k.canon(), v.canon());
        }
        format!("{{\"m\": [{pairs}]}}")
    }
}

impl<T: SudoCanon> SudoCanon for HashSet<T> {
    fn canon(&self) -> String {
        let mut items = String::new();
        for (i, x) in self.iter().enumerate() {
            if i > 0 {
                items.push_str(", ");
            }
            items.push_str(&x.canon());
        }
        format!("{{\"s\": [{items}]}}")
    }
}

impl<A: SudoCanon, B: SudoCanon> SudoCanon for (A, B) {
    fn canon(&self) -> String {
        format!("[{}, {}]", self.0.canon(), self.1.canon())
    }
}

impl<A: SudoCanon, B: SudoCanon, C: SudoCanon> SudoCanon for (A, B, C) {
    fn canon(&self) -> String {
        format!(
            "[{}, {}, {}]",
            self.0.canon(),
            self.1.canon(),
            self.2.canon()
        )
    }
}

impl<A: SudoCanon, B: SudoCanon, C: SudoCanon, D: SudoCanon> SudoCanon for (A, B, C, D) {
    fn canon(&self) -> String {
        format!(
            "[{}, {}, {}, {}]",
            self.0.canon(),
            self.1.canon(),
            self.2.canon(),
            self.3.canon()
        )
    }
}

#[inline]
pub fn canon<T: SudoCanon + ?Sized>(v: &T) -> String {
    v.canon()
}

#[inline]
pub fn sudo_assert(cond: bool, line: u32) {
    if !cond {
        trap("AssertFailed", format!("line {line}"));
    }
}

#[inline]
pub fn sudo_assert_eq<T: PartialEq + SudoCanon>(l: &T, r: &T, line: u32) {
    if l != r {
        trap(
            "AssertFailed",
            format!("line {line}: {} != {}", l.canon(), r.canon()),
        );
    }
}

// ---- expect_trap helpers ---------------------------------------------------

pub fn trap_kind_of(payload: &(dyn std::any::Any + Send)) -> Option<&'static str> {
    payload.downcast_ref::<SudoTrap>().map(|t| t.kind)
}

pub fn expect_trap_failed(line: u32, expected: &str, got: Option<&str>) -> ! {
    match got {
        None => trap(
            "AssertFailed",
            format!("line {line}: expected trap {expected}, but nothing trapped"),
        ),
        Some(k) => trap(
            "AssertFailed",
            format!("line {line}: expected trap {expected}, got {k}"),
        ),
    }
}

// ---- test runner -----------------------------------------------------------

/// Install a no-op panic hook so expected SudoTraps don't spam stderr.
pub fn silence_panic_hook() {
    panic::set_hook(Box::new(|_| {}));
}

/// Run every test; print TAP-ish lines; return exit code (nonzero on failure).
pub fn run_tests(tests: &[(&str, fn())]) -> i32 {
    silence_panic_hook();
    let mut failures = 0;
    for (i, (name, f)) in tests.iter().enumerate() {
        let result = panic::catch_unwind(AssertUnwindSafe(|| f()));
        match result {
            Ok(()) => {
                println!("ok {} - {name}", i + 1);
            }
            Err(payload) => {
                if let Some(t) = payload.downcast_ref::<SudoTrap>() {
                    failures += 1;
                    if t.detail.is_empty() {
                        println!("not ok {} - {name} [{}]", i + 1, t.kind);
                    } else {
                        println!(
                            "not ok {} - {name} [{}: {}]",
                            i + 1,
                            t.kind,
                            t.detail
                        );
                    }
                } else {
                    // A raw, unexpected Rust panic (not a SudoTrap) is a
                    // sudo backend bug, not one of the closed-set trap
                    // kinds (spec §8). It must never masquerade as a
                    // legal trap outcome in the TAP stream -- lockstep
                    // compares "not ok ... [Kind]" lines by kind string
                    // across targets, and a fabricated kind like
                    // "Unknown" could silently participate in that
                    // comparison as if it were a real trap. Report it
                    // loudly on stderr and hard-abort instead, so it
                    // surfaces as a runner crash (the harness's
                    // existing "no result (runner crashed?)" framing),
                    // never as a fake, comparable trap kind.
                    let msg = payload
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "<non-string panic payload>".to_string());
                    eprintln!(
                        "INTERNAL ERROR (sudo backend bug, not a sudo trap): test {} \"{name}\" panicked: {msg}",
                        i + 1
                    );
                    std::process::abort();
                }
            }
        }
    }
    println!("# {}/{} passed", tests.len() - failures, tests.len());
    if failures > 0 {
        1
    } else {
        0
    }
}
