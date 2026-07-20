/* The sudo C runtime. Shipped alongside generated modules by sudoc.
 * Implements the semantics pinned in spec/language.md: i64 wraparound via
 * unsigned arithmetic (no -fwrapv needed), floor division/modulo with
 * trapping, IEEE float edges, and the trap longjmp machinery.
 *
 * Every sudo heap allocation carries a small intrusive header linking it into
 * a global live list; a trap frees the entire list before longjmp-ing to the
 * boundary, so traps never leak (lockstep.md §5.2). Ordinary frees go through
 * sudo_dealloc, which unlinks in O(1).
 */
#ifndef SUDO_RT_H
#define SUDO_RT_H

#include <math.h>
#include <setjmp.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if defined(__GNUC__) || defined(__clang__)
#define SUDO_UNUSED __attribute__((unused))
#else
#define SUDO_UNUSED
#endif

typedef enum {
    SUDO_OK = 0,
    SUDO_TRAP_OUT_OF_BOUNDS,
    SUDO_TRAP_KEY_MISSING,
    SUDO_TRAP_DIV_BY_ZERO,
    SUDO_TRAP_OVERFLOW,
    SUDO_TRAP_UNWRAP_FAILED,
    SUDO_TRAP_INVALID_CONVERT,
    SUDO_TRAP_INVALID_ARG,
    SUDO_TRAP_ASSERT_FAILED,
    SUDO_TRAP_STACK_OVERFLOW
} sudo_status;

extern jmp_buf sudo_trap_jmp;
extern sudo_status sudo_trap_status;
extern int64_t sudo_trap_line;

_Noreturn void sudo_trap(sudo_status status, int64_t line);
const char *sudo_status_name(sudo_status status);
void *sudo_alloc(size_t n);
void *sudo_realloc(void *p, size_t n);
void sudo_dealloc(void *p);

/* Host-boundary text codecs (lockstep.md §5.2). Decode returns an
 * arena-tracked scalar array (traps InvalidConvert on malformed UTF-8);
 * encode returns a plain-malloc'd NUL-terminated UTF-8 string the host
 * frees with free(). */
int64_t *sudo_utf8_decode(const char *s, int64_t *out_n);
char *sudo_utf8_encode(const int64_t *xs, int64_t n);

/* Trap diagnostic detail (assert operand serialization). Display-only:
 * floats use %.17g here and repr() in Python, so formatting may differ
 * between targets — outcomes still compare by trap kind alone. */
extern char sudo_trap_detail[2048];
void sudo_det_reset(void);
void sudo_det_str(const char *s);
void sudo_det_i64(int64_t v);
void sudo_det_f64(double v);
void sudo_det_bool(bool v);

/* ---- i64 arithmetic with Overflow traps (spec §4.1) --------------------- */

static inline int64_t sudo_add(int64_t a, int64_t b) {
    int64_t r;
    if (__builtin_add_overflow(a, b, &r)) sudo_trap(SUDO_TRAP_OVERFLOW, 0);
    return r;
}
static inline int64_t sudo_sub(int64_t a, int64_t b) {
    int64_t r;
    if (__builtin_sub_overflow(a, b, &r)) sudo_trap(SUDO_TRAP_OVERFLOW, 0);
    return r;
}
static inline int64_t sudo_mul(int64_t a, int64_t b) {
    int64_t r;
    if (__builtin_mul_overflow(a, b, &r)) sudo_trap(SUDO_TRAP_OVERFLOW, 0);
    return r;
}
static inline int64_t sudo_neg(int64_t a) {
    if (a == INT64_MIN) sudo_trap(SUDO_TRAP_OVERFLOW, 0);
    return -a;
}
static inline int64_t sudo_abs(int64_t a) {
    return a < 0 ? sudo_neg(a) : a;
}
/* Floor division; (-2^63)/-1 overflows. */
static inline int64_t sudo_div(int64_t a, int64_t b) {
    if (b == 0) sudo_trap(SUDO_TRAP_DIV_BY_ZERO, 0);
    if (b == -1) return sudo_neg(a);
    int64_t q = a / b;
    if ((a % b != 0) && ((a < 0) != (b < 0))) q--;
    return q;
}
/* Floor modulo: result has the sign of the divisor. */
static inline int64_t sudo_mod(int64_t a, int64_t b) {
    if (b == 0) sudo_trap(SUDO_TRAP_DIV_BY_ZERO, 0);
    if (b == -1) return 0;
    int64_t r = a % b;
    if (r != 0 && (r < 0) != (b < 0)) r += b;
    return r;
}
static inline int64_t sudo_min_i64(int64_t a, int64_t b) { return a < b ? a : b; }
static inline int64_t sudo_max_i64(int64_t a, int64_t b) { return a > b ? a : b; }

/* ---- IEEE 754 binary64 edges (spec §4.3) -------------------------------- */

/* NaN if either operand is NaN; min(-0.0, 0.0) == -0.0 (unlike C fmin). */
static inline double sudo_fmin(double a, double b) {
    if (isnan(a) || isnan(b)) return NAN;
    if (a == b) return signbit(a) ? a : b;
    return a < b ? a : b;
}
static inline double sudo_fmax(double a, double b) {
    if (isnan(a) || isnan(b)) return NAN;
    if (a == b) return signbit(a) ? b : a;
    return a > b ? a : b;
}
/* C round() is already ties-away-from-zero, as the spec requires. */
static inline double sudo_round(double x) { return round(x); }
static inline int64_t sudo_int_of(double f) {
    if (isnan(f)) sudo_trap(SUDO_TRAP_INVALID_CONVERT, 0);
    double t = trunc(f);
    if (t < -9223372036854775808.0 || t >= 9223372036854775808.0)
        sudo_trap(SUDO_TRAP_INVALID_CONVERT, 0);
    return (int64_t)t;
}

/* Sort order for List<float>.sort(): NaN last, -0.0 before 0.0 (spec §7). */
static inline bool sudo_f64_sort_lt(double a, double b) {
    if (isnan(a)) return false;
    if (isnan(b)) return true;
    if (a == b) return signbit(a) && !signbit(b);
    return a < b;
}

static inline void sudo_assert(bool cond, int64_t line) {
    if (!cond) sudo_trap(SUDO_TRAP_ASSERT_FAILED, line);
}

/* Marks statically-unreachable function ends (e.g. after an exhaustive
 * match in which every arm returns). */
static inline _Noreturn void sudo_unreachable(void) { abort(); }

/* ---- hashing for Map/Set keys ------------------------------------------- */

static inline uint64_t sudo_hash_u64(uint64_t x) {
    x ^= x >> 33;
    x *= 0xff51afd7ed558ccdULL;
    x ^= x >> 33;
    x *= 0xc4ceb9fe1a85ec53ULL;
    x ^= x >> 33;
    return x;
}
static inline uint64_t sudo_hash_combine(uint64_t seed, uint64_t v) {
    return sudo_hash_u64(seed ^ (v + 0x9e3779b97f4a7c15ULL + (seed << 6) + (seed >> 2)));
}

#endif /* SUDO_RT_H */
