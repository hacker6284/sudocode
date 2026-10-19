//! Local interval facts for `int` expressions.
//!
//! A bound is present only when the value is proven to lie in a subrange of
//! i64. That is enough for a backend to skip an overflow check, and, when the
//! interval also fits in a safe integer (±(2^53−1)) or in i32, to pick a
//! narrower representation. Nothing here iterates to a fixed point.

use crate::IntBound;

/// Inclusive endpoints of a JS safe integer (`Number.MAX_SAFE_INTEGER`).
pub const MIN_SAFE: i64 = -9007199254740991;
pub const MAX_SAFE: i64 = 9007199254740991;

impl IntBound {
    pub fn point(n: i64) -> Self {
        Self { lo: n, hi: n }
    }

    pub fn fits_i64_pair(lo: i64, hi: i64) -> bool {
        lo <= hi
    }

    /// Every integer in the interval is a JS safe integer.
    pub fn fits_safe(&self) -> bool {
        self.lo >= MIN_SAFE && self.hi <= MAX_SAFE
    }

    /// Every integer in the interval fits in i32.
    pub fn fits_i32(&self) -> bool {
        self.lo >= i32::MIN as i64 && self.hi <= i32::MAX as i64
    }

    /// `self ⊆ lo..=hi`.
    pub fn subset_of(&self, lo: i64, hi: i64) -> bool {
        self.lo >= lo && self.hi <= hi
    }
}

fn narrow(lo: i128, hi: i128) -> Option<IntBound> {
    if lo < i64::MIN as i128 || hi > i64::MAX as i128 {
        return None;
    }
    Some(IntBound {
        lo: lo as i64,
        hi: hi as i64,
    })
}

pub fn add(a: IntBound, b: IntBound) -> Option<IntBound> {
    narrow(a.lo as i128 + b.lo as i128, a.hi as i128 + b.hi as i128)
}

pub fn sub(a: IntBound, b: IntBound) -> Option<IntBound> {
    narrow(a.lo as i128 - b.hi as i128, a.hi as i128 - b.lo as i128)
}

pub fn neg(a: IntBound) -> Option<IntBound> {
    narrow(-(a.hi as i128), -(a.lo as i128))
}

pub fn mul(a: IntBound, b: IntBound) -> Option<IntBound> {
    let mut lo = i128::MAX;
    let mut hi = i128::MIN;
    for x in [a.lo as i128, a.hi as i128] {
        for y in [b.lo as i128, b.hi as i128] {
            let p = x * y;
            lo = lo.min(p);
            hi = hi.max(p);
        }
    }
    narrow(lo, hi)
}

fn floor_div(a: i128, b: i128) -> i128 {
    let q = a / b;
    let r = a % b;
    if r != 0 && (a < 0) != (b < 0) {
        q - 1
    } else {
        q
    }
}

/// Floor division. `None` when the divisor interval contains 0 or the
/// operation can overflow (`i64::MIN / -1`). Extrema are at the corners:
/// on each side of zero, floor-div is monotonic in both arguments.
pub fn div(n: IntBound, d: IntBound) -> Option<IntBound> {
    if d.lo <= 0 && d.hi >= 0 {
        return None;
    }
    if n.lo == i64::MIN && d.lo <= -1 && d.hi >= -1 {
        return None;
    }
    let mut lo = i128::MAX;
    let mut hi = i128::MIN;
    for a in [n.lo as i128, n.hi as i128] {
        for b in [d.lo as i128, d.hi as i128] {
            let q = floor_div(a, b);
            lo = lo.min(q);
            hi = hi.max(q);
        }
    }
    narrow(lo, hi)
}

/// Floor modulo when the divisor is a non-zero constant. The result has the
/// sign of the divisor and lies in `0 .. c-1` (`c > 0`) or `c+1 ..= 0`.
pub fn modulo(n: IntBound, d: IntBound) -> Option<IntBound> {
    if d.lo != d.hi || d.lo == 0 {
        return None;
    }
    let c = d.lo;
    if c == -1 {
        return Some(IntBound::point(0));
    }
    if c > 0 {
        if n.lo >= 0 && n.hi < c {
            return Some(n);
        }
        Some(IntBound { lo: 0, hi: c - 1 })
    } else {
        Some(IntBound { lo: c + 1, hi: 0 })
    }
}
