// The sudo JavaScript runtime. Shipped alongside generated modules by sudoc.
// Implements the semantics pinned in spec/language.md: i64 with explicit
// Overflow traps (BigInt + range check), floor division with trapping, IEEE
// float edges, value-semantic deep copies, deep structural equality, and
// hashable key encodings so Lists (and anything structural) can be Map keys /
// Set elements. sudo `int` is always BigInt; sudo `float` is always number.

const I64_MIN = -(2n ** 63n);
const I64_MAX = 2n ** 63n - 1n;

export class SudoTrap extends Error {
    /** A defined runtime fault (spec §8). Kind is one of the closed set. */
    constructor(kind, detail = "") {
        super(detail ? `${kind}: ${detail}` : kind);
        this.name = "SudoTrap";
        this.kind = kind;
        this.detail = detail;
    }
}

/** Trap Overflow when a result leaves the 64-bit range (spec §4.1). */
export function chk(x) {
    if (x < I64_MIN || x > I64_MAX) {
        throw new SudoTrap("Overflow");
    }
    return x;
}

/** Floor division (toward -∞); traps DivByZero and MIN/-1 Overflow. */
export function div(a, b) {
    if (b === 0n) {
        throw new SudoTrap("DivByZero");
    }
    if (a === I64_MIN && b === -1n) {
        throw new SudoTrap("Overflow");
    }
    // JS BigInt `/` truncates toward zero; convert to floor.
    let q = a / b;
    const r = a % b;
    if (r !== 0n && (a < 0n) !== (b < 0n)) {
        q -= 1n;
    }
    return q;
}

/** Floor modulo (sign of divisor). */
export function mod_i64(a, b) {
    if (b === 0n) {
        throw new SudoTrap("DivByZero");
    }
    // JS BigInt `%` has sign of dividend; convert to floor mod.
    let r = a % b;
    if (r !== 0n && (a < 0n) !== (b < 0n)) {
        r += b;
    }
    return r;
}

export function abs_i64(x) {
    return chk(x < 0n ? -x : x);
}

export function neg(x) {
    return chk(-x);
}

export function fdiv(a, b) {
    if (b === 0.0) {
        if (a === 0.0 || Number.isNaN(a)) {
            return NaN;
        }
        // copysign(inf, a) * copysign(1, b)
        const sa = Object.is(a, -0) || a < 0 ? -1 : 1;
        const sb = Object.is(b, -0) || b < 0 ? -1 : 1;
        return sa * sb * Infinity;
    }
    return a / b;
}

export function fmin(a, b) {
    if (Number.isNaN(a) || Number.isNaN(b)) {
        return NaN;
    }
    if (a === b) {
        // min(-0.0, 0.0) == -0.0
        const sa = Object.is(a, -0) || (a < 0 && !Object.is(a, 0)) ? -1 : 1;
        const sb = Object.is(b, -0) || (b < 0 && !Object.is(b, 0)) ? -1 : 1;
        // Prefer the one with the more-negative sign bit when equal in magnitude.
        if (Object.is(a, -0) || Object.is(b, -0)) {
            return Object.is(a, -0) ? a : b;
        }
        return sa < sb ? a : b;
    }
    return a < b ? a : b;
}

export function fmax(a, b) {
    if (Number.isNaN(a) || Number.isNaN(b)) {
        return NaN;
    }
    if (a === b) {
        if (Object.is(a, -0) || Object.is(b, -0)) {
            return Object.is(a, -0) ? b : a;
        }
        return a;
    }
    return a > b ? a : b;
}

export function floor(x) {
    if (Number.isNaN(x) || !Number.isFinite(x)) {
        return x;
    }
    return Math.floor(x);
}

export function ceil(x) {
    if (Number.isNaN(x) || !Number.isFinite(x)) {
        return x;
    }
    return Math.ceil(x);
}

/** Ties away from zero (spec §4.3), not JS Math.round (half toward +Inf). */
export function round_half_away(x) {
    if (Number.isNaN(x) || !Number.isFinite(x)) {
        return x;
    }
    if (x >= 0) {
        return Math.floor(x + 0.5);
    }
    return Math.ceil(x - 0.5);
}

export function sqrt(x) {
    if (Number.isNaN(x) || x < 0.0) {
        return NaN;
    }
    return Math.sqrt(x);
}

export function int_of(x) {
    if (Number.isNaN(x) || !Number.isFinite(x)) {
        throw new SudoTrap("InvalidConvert", "NaN or infinity to int");
    }
    const t = Math.trunc(x);
    // Convert then range-check in BigInt space (robust for huge finite doubles).
    const bi = BigInt(t);
    if (bi < I64_MIN || bi > I64_MAX) {
        throw new SudoTrap("InvalidConvert", "float out of int range");
    }
    return bi;
}

// ---- Option / Result -------------------------------------------------------

export class Some {
    constructor(value) {
        this.value = value;
    }
}

export class NoneOpt {
    constructor() {}
}

export const NONE = new NoneOpt();

export class Ok {
    constructor(value) {
        this.value = value;
    }
}

export class Err {
    constructor(error) {
        this.error = error;
    }
}

export function is_some(o) {
    return o instanceof Some;
}

export function is_ok(r) {
    return r instanceof Ok;
}

export function is_err(r) {
    return r instanceof Err;
}

export function is_none(o) {
    return o instanceof NoneOpt;
}

export function unwrap(o) {
    if (o instanceof Some) {
        return o.value;
    }
    if (o instanceof Ok) {
        return o.value;
    }
    throw new SudoTrap("UnwrapFailed");
}

export function get_or(o, default_) {
    if (o instanceof Some) {
        return o.value;
    }
    if (o instanceof Ok) {
        return o.value;
    }
    return default_;
}

// ---- value semantics -------------------------------------------------------

export function dup(v) {
    if (Array.isArray(v)) {
        return v.map(dup);
    }
    if (v instanceof SudoMap) {
        return v._dup();
    }
    if (v instanceof SudoSet) {
        return v._dup();
    }
    if (v instanceof Some) {
        return new Some(dup(v.value));
    }
    if (v instanceof Ok) {
        return new Ok(dup(v.value));
    }
    if (v instanceof Err) {
        return new Err(dup(v.error));
    }
    // Records / enum variants: classes with static _sudoKind.
    if (v && typeof v === "object" && v.constructor && v.constructor._sudoKind) {
        const cls = v.constructor;
        const fields = cls._sudoFields || [];
        return new cls(...fields.map((f) => dup(v[f])));
    }
    return v;
}

/**
 * Deep structural equality with IEEE float semantics (NaN != NaN).
 * Walks structures explicitly — never relies on === for composites.
 * Bool is JS boolean, int is BigInt; they never conflate, so no bool-vs-int
 * identity branch is needed (unlike Python where bool is a subclass of int).
 */
export function eq(a, b) {
    if (typeof a === "number" || typeof b === "number") {
        return typeof a === "number" && typeof b === "number" && a === b;
    }
    if (typeof a === "boolean" || typeof b === "boolean") {
        return a === b;
    }
    if (typeof a === "bigint" && typeof b === "bigint") {
        return a === b;
    }
    if (Array.isArray(a) && Array.isArray(b)) {
        if (a.length !== b.length) {
            return false;
        }
        for (let i = 0; i < a.length; i++) {
            if (!eq(a[i], b[i])) {
                return false;
            }
        }
        return true;
    }
    if (a instanceof SudoMap && b instanceof SudoMap) {
        if (a.size !== b.size) {
            return false;
        }
        for (const [k, v] of a.pairs()) {
            const other = b.get_opt(k);
            if (other instanceof NoneOpt || !eq(v, other.value)) {
                return false;
            }
        }
        return true;
    }
    if (a instanceof SudoSet && b instanceof SudoSet) {
        if (a.size !== b.size) {
            return false;
        }
        for (const x of a.items_list()) {
            if (!b.has(x)) {
                return false;
            }
        }
        return true;
    }
    if (a instanceof NoneOpt && b instanceof NoneOpt) {
        return true;
    }
    if (a instanceof Some && b instanceof Some) {
        return eq(a.value, b.value);
    }
    if (a instanceof Ok && b instanceof Ok) {
        return eq(a.value, b.value);
    }
    if (a instanceof Err && b instanceof Err) {
        return eq(a.error, b.error);
    }
    if (
        a &&
        b &&
        typeof a === "object" &&
        typeof b === "object" &&
        a.constructor &&
        a.constructor._sudoKind &&
        b.constructor &&
        b.constructor._sudoKind
    ) {
        if (a.constructor !== b.constructor) {
            return false;
        }
        const fields = a.constructor._sudoFields || [];
        for (const f of fields) {
            if (!eq(a[f], b[f])) {
                return false;
            }
        }
        return true;
    }
    return false;
}

/**
 * Immutable, comparable encoding of a (hashable-typed) sudo value.
 * Stringified so native Map/Set can key by structural equality.
 * Floats are never valid map/set keys per backend-guide.md §4.8.
 */
export function key_form(v) {
    return JSON.stringify(key_form_raw(v));
}

function key_form_raw(v) {
    if (typeof v === "bigint") {
        return ["i", v.toString()];
    }
    if (typeof v === "boolean") {
        return ["b", v];
    }
    if (typeof v === "number") {
        // Not expected as a key; encode stably if seen.
        if (Number.isNaN(v)) {
            return ["f", "NaN"];
        }
        if (!Number.isFinite(v)) {
            return ["f", v > 0 ? "Inf" : "-Inf"];
        }
        if (Object.is(v, -0)) {
            return ["f", "-0"];
        }
        return ["f", String(v)];
    }
    if (Array.isArray(v)) {
        return ["a", v.map(key_form_raw)];
    }
    if (v instanceof Some) {
        return ["Some", key_form_raw(v.value)];
    }
    if (v instanceof NoneOpt) {
        return ["None"];
    }
    if (v instanceof Ok) {
        return ["Ok", key_form_raw(v.value)];
    }
    if (v instanceof Err) {
        return ["Err", key_form_raw(v.error)];
    }
    if (v && typeof v === "object" && v.constructor && v.constructor._sudoKind) {
        const fields = v.constructor._sudoFields || [];
        return [v.constructor.name, ...fields.map((f) => key_form_raw(v[f]))];
    }
    return ["?", String(v)];
}

// ---- containers ------------------------------------------------------------

/** Bounds-check in BigInt space, then Number() for the array index. */
function idx(a, i) {
    const n = BigInt(a.length);
    if (i < 0n || i >= n) {
        throw new SudoTrap("OutOfBounds", `index ${i} of length ${a.length}`);
    }
    return Number(i);
}

export function at(a, i) {
    return a[idx(a, i)];
}

export function put(a, i, v) {
    a[idx(a, i)] = v;
}

export function pop(a) {
    if (a.length === 0) {
        throw new SudoTrap("OutOfBounds", "pop from empty list");
    }
    return a.pop();
}

export function insert(a, i, v) {
    const n = BigInt(a.length);
    if (i < 0n || i > n) {
        throw new SudoTrap("OutOfBounds", `insert at ${i} of length ${a.length}`);
    }
    a.splice(Number(i), 0, v);
}

export function remove_at(a, i) {
    const j = idx(a, i);
    return a.splice(j, 1)[0];
}

export function swap(a, i, j) {
    const n = BigInt(a.length);
    if (i < 0n || i >= n || j < 0n || j >= n) {
        throw new SudoTrap("OutOfBounds", `swap ${i},${j} of length ${a.length}`);
    }
    const ii = Number(i);
    const jj = Number(j);
    const tmp = a[ii];
    a[ii] = a[jj];
    a[jj] = tmp;
}

function sort_key(x) {
    if (typeof x === "number") {
        if (Number.isNaN(x)) {
            return [2, 0, 0];
        }
        const sign = Object.is(x, -0) || x < 0 ? -1 : 1;
        return [1, x, sign];
    }
    // BigInt / other: compare via value; secondary 0.
    return [1, x, 0];
}

/** Ascending stable sort; floats order NaN last, -0.0 before 0.0. */
export function sort(a) {
    a.sort((x, y) => {
        const kx = sort_key(x);
        const ky = sort_key(y);
        if (kx[0] !== ky[0]) {
            return kx[0] < ky[0] ? -1 : 1;
        }
        // value compare
        if (kx[1] < ky[1]) {
            return -1;
        }
        if (kx[1] > ky[1]) {
            return 1;
        }
        // sign tie-break for ±0
        if (kx[2] < ky[2]) {
            return -1;
        }
        if (kx[2] > ky[2]) {
            return 1;
        }
        return 0;
    });
}

export function filled(n, v) {
    if (n < 0n) {
        throw new SudoTrap("InvalidArg", `filled(${n})`);
    }
    // Cap to a safe Number length; n is non-negative BigInt within i64.
    const count = Number(n);
    const out = new Array(count);
    for (let i = 0; i < count; i++) {
        out[i] = dup(v);
    }
    return out;
}

/** Text literal: already a list of Unicode scalar BigInt values from the IR. */
export function text_from_scalars(scalars) {
    return scalars.slice();
}

export class SudoMap {
    /**
     * Insertion-ordered (Map-backed) — order is unspecified by the language.
     * Keys are stored by structural key_form so Lists and records can be keys;
     * original key values are retained for iteration.
     */
    constructor() {
        this._d = new Map();
    }

    get size() {
        return this._d.size;
    }

    has(k) {
        return this._d.has(key_form(k));
    }

    get(k) {
        const kf = key_form(k);
        if (!this._d.has(kf)) {
            throw new SudoTrap("KeyMissing");
        }
        return this._d.get(kf)[1];
    }

    set(k, v) {
        this._d.set(key_form(k), [dup(k), v]);
    }

    get_opt(k) {
        const kf = key_form(k);
        if (this._d.has(kf)) {
            return new Some(this._d.get(kf)[1]);
        }
        return NONE;
    }

    delete(k) {
        const kf = key_form(k);
        if (this._d.has(kf)) {
            this._d.delete(kf);
            return true;
        }
        return false;
    }

    keys_list() {
        const out = [];
        for (const [k] of this._d.values()) {
            out.push(dup(k));
        }
        return out;
    }

    values_list() {
        const out = [];
        for (const [, v] of this._d.values()) {
            out.push(v);
        }
        return out;
    }

    pairs() {
        const out = [];
        for (const [k, v] of this._d.values()) {
            out.push([k, v]);
        }
        return out;
    }

    _dup() {
        const m = new SudoMap();
        for (const [k, v] of this._d.values()) {
            m._d.set(key_form(k), [dup(k), dup(v)]);
        }
        return m;
    }
}

export class SudoSet {
    constructor() {
        this._d = new Map();
    }

    get size() {
        return this._d.size;
    }

    has(v) {
        return this._d.has(key_form(v));
    }

    add(v) {
        const kf = key_form(v);
        if (this._d.has(kf)) {
            return false;
        }
        this._d.set(kf, dup(v));
        return true;
    }

    remove(v) {
        const kf = key_form(v);
        if (this._d.has(kf)) {
            this._d.delete(kf);
            return true;
        }
        return false;
    }

    items_list() {
        const out = [];
        for (const v of this._d.values()) {
            out.push(dup(v));
        }
        return out;
    }

    _dup() {
        const s = new SudoSet();
        for (const [k, v] of this._d.entries()) {
            s._d.set(k, dup(v));
        }
        return s;
    }
}

// ---- tests -----------------------------------------------------------------

export function sudo_assert(cond, line) {
    if (!cond) {
        throw new SudoTrap("AssertFailed", `line ${line}`);
    }
}

/**
 * Canonical display serialization (lockstep.md §4). Diagnostic-only.
 * Shape mirrors Python's canon for shared diagnostic value.
 * Integral floats render as "N.0" to match Python's repr(1.0) more closely.
 */
export function canon(v) {
    if (typeof v === "boolean") {
        return v ? "true" : "false";
    }
    if (typeof v === "bigint") {
        return v.toString();
    }
    if (typeof v === "number") {
        let s;
        if (Number.isNaN(v)) {
            s = "NaN";
        } else if (!Number.isFinite(v)) {
            s = v > 0 ? "Inf" : "-Inf";
        } else if (Object.is(v, -0)) {
            s = "-0.0";
        } else {
            s = String(v);
            // Force trailing .0 for integral floats (Python repr(1.0) == "1.0").
            if (/^-?\d+$/.test(s)) {
                s = s + ".0";
            }
        }
        return `{"f": "${s}"}`;
    }
    if (Array.isArray(v)) {
        return "[" + v.map(canon).join(", ") + "]";
    }
    if (v instanceof SudoMap) {
        const pairs = v
            .pairs()
            .map(([k, x]) => `[${canon(k)}, ${canon(x)}]`)
            .join(", ");
        return `{"m": [${pairs}]}`;
    }
    if (v instanceof SudoSet) {
        return `{"s": [${v.items_list().map(canon).join(", ")}]}`;
    }
    if (v instanceof Some) {
        return `{"e": "Option.Some", "v": [${canon(v.value)}]}`;
    }
    if (v instanceof NoneOpt) {
        return `{"e": "Option.None"}`;
    }
    if (v instanceof Ok) {
        return `{"e": "Result.Ok", "v": [${canon(v.value)}]}`;
    }
    if (v instanceof Err) {
        return `{"e": "Result.Err", "v": [${canon(v.error)}]}`;
    }
    if (v && typeof v === "object" && v.constructor && v.constructor._sudoKind) {
        const [kind, name] = v.constructor._sudoKind;
        const fields = v.constructor._sudoFields || [];
        const vals = fields.map((f) => canon(v[f])).join(", ");
        if (vals) {
            return `{"${kind}": "${name}", "v": [${vals}]}`;
        }
        return `{"${kind}": "${name}"}`;
    }
    return String(v);
}

export function sudo_assert_eq(l, r, line) {
    if (!eq(l, r)) {
        throw new SudoTrap("AssertFailed", `line ${line}: ${canon(l)} != ${canon(r)}`);
    }
}

/**
 * Run every test; print TAP-ish lines; return exit code.
 * `tests` is an array of [name, fn] pairs in declaration order.
 */
export function run_tests(tests) {
    let failures = 0;
    for (let i = 0; i < tests.length; i++) {
        const [name, fn] = tests[i];
        try {
            fn();
            console.log(`ok ${i + 1} - ${name}`);
        } catch (e) {
            failures += 1;
            if (e instanceof SudoTrap) {
                const detail = e.detail ? `: ${e.detail}` : "";
                console.log(`not ok ${i + 1} - ${name} [${e.kind}${detail}]`);
            } else if (
                e instanceof RangeError &&
                typeof e.message === "string" &&
                e.message.toLowerCase().includes("call stack")
            ) {
                console.log(`not ok ${i + 1} - ${name} [StackOverflow]`);
            } else {
                const msg = e && e.message ? e.message : String(e);
                console.log(`not ok ${i + 1} - ${name} [Unknown: ${msg}]`);
            }
        }
    }
    console.log(`# ${tests.length - failures}/${tests.length} passed`);
    return failures ? 1 : 0;
}
