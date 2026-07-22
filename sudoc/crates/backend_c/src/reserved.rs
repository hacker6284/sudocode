//! Escape target-reserved identifiers so emitted C stays valid, and so a
//! user function can never silently collide with a libc symbol pulled in
//! by sudo_rt.h's own includes (F6 + F9 — unified: same reserved-namespace
//! escape, applied to every user-identifier-bearing IR field).

use sudoc_ir::{IrExpr, IrExprKind, IrModule, IrPattern, IrStmt, Place, Ty};

const RESERVED: &[&str] = &[
    // C11/C17 keywords.
    "auto", "break", "case", "char", "const", "continue", "default", "do",
    "double", "else", "enum", "extern", "float", "for", "goto", "if",
    "inline", "int", "long", "register", "restrict", "return", "short",
    "signed", "sizeof", "static", "struct", "switch", "typedef", "union",
    "unsigned", "void", "volatile", "while",
    "_Alignas", "_Alignof", "_Atomic", "_Bool", "_Complex", "_Generic",
    "_Imaginary", "_Noreturn", "_Static_assert", "_Thread_local",
    // sudo_rt.h includes <stdbool.h>, whose bool/true/false are macros
    // (and are hard keywords as of C23) — a user identifier with one of
    // these names would either be a macro-substitution corruption or a
    // straight keyword clash.
    "bool", "true", "false",
    // Fixed-width integer typedefs from <stdint.h> that this backend's
    // own codegen emits as C *type* tokens throughout (see `c_type` in
    // types_gen.rs) — a same-named user identifier collides the same way
    // a keyword does (e.g. `int64_t int64_t = 5;`).
    "int64_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "size_t",
    // <setjmp.h> (trap handling).
    "setjmp", "longjmp",
    // <stdlib.h> — arithmetic/allocation/conversion surface.
    "abs", "labs", "llabs", "div", "ldiv", "lldiv", "atof", "atoi", "atol",
    "atoll", "strtod", "strtof", "strtold", "strtol", "strtoll", "strtoul",
    "strtoull", "rand", "srand", "malloc", "calloc", "realloc", "free",
    "aligned_alloc", "abort", "atexit", "exit", "getenv", "system",
    "bsearch", "qsort",
    // <string.h>.
    "memcpy", "memmove", "memset", "memcmp", "memchr", "strcpy", "strncpy",
    "strcat", "strncat", "strcmp", "strncmp", "strcoll", "strxfrm",
    "strchr", "strrchr", "strspn", "strcspn", "strpbrk", "strstr",
    "strtok", "strerror", "strlen",
    // <stdio.h>.
    "printf", "fprintf", "sprintf", "snprintf", "vprintf", "vfprintf",
    "vsprintf", "vsnprintf", "scanf", "fscanf", "sscanf", "fopen",
    "freopen", "fclose", "fflush", "setbuf", "setvbuf", "fread", "fwrite",
    "fgetc", "getc", "fgets", "fputc", "putc", "fputs", "getchar",
    "putchar", "puts", "ungetc", "fseek", "ftell", "rewind", "fgetpos",
    "fsetpos", "clearerr", "feof", "ferror", "perror", "remove", "rename",
    "tmpfile", "tmpnam",
    // <math.h>.
    "fabs", "fmod", "remainder", "fma", "fmax", "fmin", "fdim", "nan",
    "exp", "exp2", "expm1", "log", "log10", "log2", "log1p", "pow",
    "sqrt", "cbrt", "hypot", "sin", "cos", "tan", "asin", "acos", "atan",
    "atan2", "sinh", "cosh", "tanh", "asinh", "acosh", "atanh", "erf",
    "erfc", "tgamma", "lgamma", "ceil", "floor", "trunc", "round",
    "lround", "llround", "rint", "lrint", "llrint", "nearbyint", "frexp",
    "ldexp", "modf", "scalbn", "ilogb", "logb", "nextafter", "copysign",
    // The generated program's own always-present, un-shadowable test-
    // runner entry point (`pub fn emit` in src/lib.rs emits a literal
    // top-level `int main(void) { ... }` whenever `with_tests` is set).
    "main",
];

/// Injective, reserved-namespace escape for a colliding user identifier.
/// `sudo_k` + a length-prefixed encoding of `name` can never collide with
/// any other user identifier (users cannot declare names starting with
/// `sudo_`, checker-enforced — see sudoc_ir::mangle module docs) and is
/// injective (mangle::enc is length-prefixed / self-delimiting), so two
/// different colliding names always escape to two different safe names,
/// and the same colliding name always escapes to the same safe name
/// wherever it appears (declaration and every reference).
pub fn escape(name: &str) -> String {
    format!("sudo_k{}", sudoc_ir::mangle::enc(name))
}

fn resolve(name: &str) -> String {
    if RESERVED.contains(&name) {
        escape(name)
    } else {
        name.to_string()
    }
}

fn resolve_qualified(name: &str) -> String {
    match name.split_once('.') {
        Some((m, local)) => format!("{m}.{}", resolve(local)),
        None => resolve(name),
    }
}

pub fn rename_reserved(m: &IrModule) -> IrModule {
    let mut m = m.clone();
    for r in &mut m.records {
        r.name = resolve(&r.name);
        for (fname, fty) in &mut r.fields {
            *fname = resolve(fname);
            ty(fty);
        }
    }
    for e in &mut m.enums {
        e.name = resolve(&e.name);
        for v in &mut e.variants {
            v.name = resolve(&v.name);
            for (fname, fty) in &mut v.fields {
                *fname = resolve(fname);
                ty(fty);
            }
        }
    }
    for c in &mut m.consts {
        c.name = resolve(&c.name);
        ty(&mut c.ty);
        expr(&mut c.value);
    }
    for f in &mut m.funcs {
        f.name = resolve(&f.name); // declaration: never dotted
        for p in &mut f.params {
            p.name = resolve(&p.name);
            ty(&mut p.ty);
        }
        if let Some(t) = &mut f.ret {
            ty(t);
        }
        for s in &mut f.body {
            stmt(s);
        }
    }
    for t in &mut m.tests {
        for s in &mut t.body {
            stmt(s);
        }
    }
    m
}

fn ty(t: &mut Ty) {
    match t {
        Ty::Record(n) | Ty::Enum(n) => *n = resolve(n),
        Ty::List(e) | Ty::Set(e) | Ty::Option_(e) => ty(e),
        Ty::Map(k, v) => {
            ty(k);
            ty(v);
        }
        Ty::Result_(a, b) => {
            ty(a);
            ty(b);
        }
        Ty::Tuple(ts) => ts.iter_mut().for_each(ty),
        Ty::Func { params, ret } => {
            params.iter_mut().for_each(ty);
            if let Some(r) = ret {
                ty(r);
            }
        }
        _ => {}
    }
}

fn place(p: &mut Place) {
    match p {
        Place::Var(n) => *n = resolve(n),
        Place::Index { base, base_ty, index } => {
            place(base);
            ty(base_ty);
            expr(index);
        }
        Place::Field { base, base_ty, name } => {
            place(base);
            ty(base_ty);
            *name = resolve(name);
        }
    }
}

fn block(stmts: &mut [IrStmt]) {
    for s in stmts {
        stmt(s);
    }
}

fn stmt(s: &mut IrStmt) {
    match s {
        IrStmt::Assign { target, value, .. } => {
            place(target);
            expr(value);
        }
        IrStmt::TupleAssign { targets, value, .. } => {
            for t in targets.iter_mut() {
                *t = resolve(t);
            }
            expr(value);
        }
        IrStmt::Expr(e) => expr(e),
        IrStmt::If { arms, else_block } => {
            for (c, b) in arms {
                expr(c);
                block(b);
            }
            if let Some(b) = else_block {
                block(b);
            }
        }
        IrStmt::While { cond, body } => {
            expr(cond);
            block(body);
        }
        IrStmt::ForRange { var, from, to, body, .. } => {
            *var = resolve(var);
            expr(from);
            expr(to);
            block(body);
        }
        IrStmt::ForIn { vars, iter, body } => {
            for v in vars.iter_mut() {
                *v = resolve(v);
            }
            expr(iter);
            block(body);
        }
        IrStmt::Match { scrutinee, arms } => {
            expr(scrutinee);
            for a in arms {
                if let IrPattern::Variant { enum_name, variant, binders } = &mut a.pattern {
                    if enum_name != "Option" && enum_name != "Result" {
                        *enum_name = resolve(enum_name);
                        *variant = resolve(variant);
                    }
                    for b in binders.iter_mut() {
                        *b = resolve(b);
                    }
                }
                block(&mut a.body);
            }
        }
        IrStmt::Return(Some(e)) => expr(e),
        IrStmt::Assert { cond, .. } => expr(cond),
        IrStmt::ExpectTrap { body, .. } => block(body),
        IrStmt::Return(None) | IrStmt::Skip | IrStmt::Break | IrStmt::Continue => {}
    }
}

fn expr(e: &mut IrExpr) {
    ty(&mut e.ty);
    match &mut e.kind {
        IrExprKind::Local(n) => *n = resolve(n), // never dotted
        IrExprKind::Const(n) => *n = resolve_qualified(n),
        IrExprKind::FuncRef(n) => *n = resolve_qualified(n),
        IrExprKind::CallFunc { name, args } => {
            *name = resolve_qualified(name);
            args.iter_mut().for_each(expr);
        }
        IrExprKind::CallValue { callee, args } => {
            expr(callee);
            args.iter_mut().for_each(expr);
        }
        IrExprKind::NewRecord { name, args } => {
            *name = resolve(name);
            args.iter_mut().for_each(expr);
        }
        IrExprKind::NewVariant { enum_name, variant, args } => {
            if enum_name != "Option" && enum_name != "Result" {
                *enum_name = resolve(enum_name);
                *variant = resolve(variant);
            }
            args.iter_mut().for_each(expr);
        }
        IrExprKind::List(xs) | IrExprKind::Tuple(xs) => xs.iter_mut().for_each(expr),
        IrExprKind::Builtin { args, .. } => args.iter_mut().for_each(expr),
        IrExprKind::MutBuiltin { recv, recv_ty, args, .. } => {
            place(recv);
            ty(recv_ty);
            args.iter_mut().for_each(expr);
        }
        IrExprKind::GetField { recv, name } => {
            expr(recv);
            *name = resolve(name);
        }
        IrExprKind::Index { recv, index } => {
            expr(recv);
            expr(index);
        }
        IrExprKind::Unary { operand, .. } => expr(operand),
        IrExprKind::Binary { lhs, rhs, .. } => {
            expr(lhs);
            expr(rhs);
        }
        _ => {}
    }
}
