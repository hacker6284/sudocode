//! Host-boundary adapters for C (lockstep.md §5.2): for each adaptable
//! export, a wrapper `sudo_status {module}_{fn}(params..., outs...)` that
//! setjmps, converts host values in, calls the internal function, and copies
//! results into plain-malloc'd out-parameters (host frees with `free()`).
//!
//! Adaptable v1 surface: int, float, bool, text, `List<int>`, `List<float>`
//! (inout Lists become an in-array plus an out-array pair). Exports using
//! other types are declared in the header comment as internal-API-only.

use std::fmt::Write;

use sudoc_ir::{BoundaryTy, IrFunc, IrModule};

/// The C boundary shape of one sudo surface type.
enum Shape {
    Int,
    Float,
    Bool,
    Text,
    ListInt,
    ListFloat,
}

fn shape(te: &BoundaryTy) -> Option<Shape> {
    match te {
        BoundaryTy::Int => Some(Shape::Int),
        BoundaryTy::Float => Some(Shape::Float),
        BoundaryTy::Bool => Some(Shape::Bool),
        BoundaryTy::Text => Some(Shape::Text),
        BoundaryTy::List(t) => match **t {
            BoundaryTy::Int => Some(Shape::ListInt),
            BoundaryTy::Float => Some(Shape::ListFloat),
            _ => None,
        },
        _ => None,
    }
}

impl Shape {
    fn scalar_c(&self) -> Option<&'static str> {
        match self {
            Shape::Int => Some("int64_t"),
            Shape::Float => Some("double"),
            Shape::Bool => Some("bool"),
            _ => None,
        }
    }
    fn elem_c(&self) -> &'static str {
        match self {
            Shape::ListFloat => "double",
            _ => "int64_t",
        }
    }
    fn list_ty(&self) -> &'static str {
        match self {
            Shape::ListFloat => "List_f64",
            _ => "List_i64",
        }
    }
}

/// Is this export adaptable, i.e. every param and the return have a shape?
fn adaptable(f: &IrFunc) -> bool {
    f.params.iter().all(|p| {
        shape(&p.boundary).is_some_and(|s| !p.inout || matches!(s, Shape::ListInt | Shape::ListFloat))
    }) && f.ret_boundary.as_ref().map(|r| shape(r).is_some()).unwrap_or(true)
}

pub(crate) fn wrapped_exports(m: &IrModule) -> Vec<String> {
    m.funcs
        .iter()
        .filter(|f| f.export && adaptable(f))
        .map(|f| f.name.clone())
        .collect()
}

/// One wrapper's C signature: `sudo_status {module}_{name}(...)`.
fn signature(m: &IrModule, f: &IrFunc) -> String {
    let mut args: Vec<String> = Vec::new();
    for p in &f.params {
        match shape(&p.boundary).expect("adaptable") {
            Shape::Int => args.push(format!("int64_t {}", p.name)),
            Shape::Float => args.push(format!("double {}", p.name)),
            Shape::Bool => args.push(format!("bool {}", p.name)),
            Shape::Text => args.push(format!("const char *{}", p.name)),
            s @ (Shape::ListInt | Shape::ListFloat) => {
                args.push(format!("const {} *{}", s.elem_c(), p.name));
                args.push(format!("int64_t {}_n", p.name));
                if p.inout {
                    args.push(format!("{} **{}_out", s.elem_c(), p.name));
                    args.push(format!("int64_t *{}_out_n", p.name));
                }
            }
        }
    }
    if let Some(rb) = &f.ret_boundary {
        match shape(rb).expect("adaptable") {
            Shape::Int => args.push("int64_t *out".into()),
            Shape::Float => args.push("double *out".into()),
            Shape::Bool => args.push("bool *out".into()),
            Shape::Text => args.push("char **out".into()),
            s @ (Shape::ListInt | Shape::ListFloat) => {
                args.push(format!("{} **out", s.elem_c()));
                args.push("int64_t *out_n".into());
            }
        }
    }
    let arglist = if args.is_empty() { "void".to_string() } else { args.join(", ") };
    format!("sudo_status {}_{}({})", m.name, f.name, arglist)
}

/// Emit the wrapper function bodies (appended to the translation unit).
pub(crate) fn emit_wrappers(m: &IrModule, out: &mut String) {
    let exports: Vec<&IrFunc> = m.funcs.iter().filter(|f| f.export && adaptable(f)).collect();
    if exports.is_empty() {
        return;
    }
    let _ = writeln!(out, "/* ---- host boundary (see {}.h) ---- */\n", m.name);
    for f in exports {
        let _ = writeln!(out, "{} {{", signature(m, f));
        let _ = writeln!(out, "    if (setjmp(sudo_trap_jmp) != 0) return sudo_trap_status;");
        // Convert inputs to internal values.
        let mut call_args: Vec<String> = Vec::new();
        for p in &f.params {
            match shape(&p.boundary).expect("adaptable") {
                Shape::Int | Shape::Float | Shape::Bool => {
                    if p.inout {
                        unreachable!("checker forbids scalar inout exports");
                    }
                    call_args.push(p.name.clone());
                }
                Shape::Text => {
                    let _ = writeln!(out, "    int64_t _{0}_n = 0;", p.name);
                    let _ = writeln!(
                        out,
                        "    int64_t *_{0}_buf = sudo_utf8_decode({0}, &_{0}_n);",
                        p.name
                    );
                    let _ = writeln!(
                        out,
                        "    List_i64 _{0} = List_i64_from(_{0}_buf, _{0}_n);",
                        p.name
                    );
                    let _ = writeln!(out, "    sudo_dealloc(_{0}_buf);", p.name);
                    call_args.push(format!("_{}", p.name));
                }
                s @ (Shape::ListInt | Shape::ListFloat) => {
                    let _ = writeln!(
                        out,
                        "    {} _{} = {}_from({}, {}_n);",
                        s.list_ty(),
                        p.name,
                        s.list_ty(),
                        p.name,
                        p.name
                    );
                    call_args.push(if p.inout {
                        format!("&_{}", p.name)
                    } else {
                        format!("_{}", p.name)
                    });
                }
            }
        }
        // Call the internal function.
        let call = format!("{}({})", f.name, call_args.join(", "));
        if f.ret.is_some() {
            let rb = f.ret_boundary.as_ref().expect("surface return type");
            match shape(rb).expect("adaptable") {
                s @ (Shape::Int | Shape::Float | Shape::Bool) => {
                    let _ = writeln!(out, "    {} _r = {call};", s.scalar_c().unwrap());
                }
                Shape::Text => {
                    let _ = writeln!(out, "    List_i64 _r = {call};");
                }
                Shape::ListInt => {
                    let _ = writeln!(out, "    List_i64 _r = {call};");
                }
                Shape::ListFloat => {
                    let _ = writeln!(out, "    List_f64 _r = {call};");
                }
            }
        } else {
            let _ = writeln!(out, "    {call};");
        }
        // Write inout results into the host's out pairs.
        for p in f.params.iter().filter(|p| p.inout) {
            let s = shape(&p.boundary).expect("adaptable");
            let et = s.elem_c();
            let _ = writeln!(
                out,
                "    *{0}_out = malloc(_{0}.len ? (size_t)_{0}.len * sizeof({et}) : 1);",
                p.name
            );
            let _ = writeln!(
                out,
                "    if (!*{0}_out) {{ fprintf(stderr, \"sudo: out of memory\\n\"); abort(); }}",
                p.name
            );
            let _ = writeln!(
                out,
                "    memcpy(*{0}_out, _{0}.data, (size_t)_{0}.len * sizeof({et}));",
                p.name
            );
            let _ = writeln!(out, "    *{0}_out_n = _{0}.len;", p.name);
        }
        // Free internal copies of non-inout composite params? The callee
        // owns and frees its parameters; nothing to do here.
        for p in f.params.iter().filter(|p| p.inout) {
            let s = shape(&p.boundary).expect("adaptable");
            let _ = writeln!(out, "    {}_free(&_{});", s.list_ty(), p.name);
        }
        // Convert the return value out.
        if f.ret.is_some() {
            let rb = f.ret_boundary.as_ref().unwrap();
            match shape(rb).expect("adaptable") {
                Shape::Int | Shape::Float | Shape::Bool => {
                    let _ = writeln!(out, "    *out = _r;");
                }
                Shape::Text => {
                    let _ = writeln!(out, "    *out = sudo_utf8_encode(_r.data, _r.len);");
                    let _ = writeln!(out, "    List_i64_free(&_r);");
                }
                s @ (Shape::ListInt | Shape::ListFloat) => {
                    let et = s.elem_c();
                    let _ = writeln!(
                        out,
                        "    *out = malloc(_r.len ? (size_t)_r.len * sizeof({et}) : 1);"
                    );
                    let _ = writeln!(
                        out,
                        "    if (!*out) {{ fprintf(stderr, \"sudo: out of memory\\n\"); abort(); }}"
                    );
                    let _ = writeln!(out, "    memcpy(*out, _r.data, (size_t)_r.len * sizeof({et}));");
                    let _ = writeln!(out, "    *out_n = _r.len;");
                    let _ = writeln!(out, "    {}_free(&_r);", s.list_ty());
                }
            }
        }
        let _ = writeln!(out, "    return SUDO_OK;");
        let _ = writeln!(out, "}}\n");
    }
}

/// The host-facing header, or None when nothing is adaptable.
pub fn emit_header(m: &IrModule) -> Option<String> {
    let adapted: Vec<&IrFunc> = m.funcs.iter().filter(|f| f.export && adaptable(f)).collect();
    if adapted.is_empty() {
        return None;
    }
    let skipped: Vec<&IrFunc> =
        m.funcs.iter().filter(|f| f.export && !adaptable(f)).collect();
    let mut out = String::new();
    let guard = format!("{}_SUDO_H", m.name.to_uppercase());
    let _ = writeln!(out, "/* Host API for {}.sudo — generated by sudoc.", m.name);
    let _ = writeln!(out, " * Every call returns SUDO_OK or the trap kind that aborted it.");
    let _ = writeln!(out, " * Out-buffers (char*, arrays) are malloc'd; free() them when done.");
    let _ = writeln!(out, " * Not thread-safe: calls share one trap jump buffer. */");
    let _ = writeln!(out, "#ifndef {guard}");
    let _ = writeln!(out, "#define {guard}");
    let _ = writeln!(out);
    let _ = writeln!(out, "#include \"sudo_rt.h\"");
    let _ = writeln!(out);
    for f in &adapted {
        let _ = writeln!(out, "{};", signature(m, f));
    }
    if !skipped.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "/* Exports without a portable C mapping yet (use the internal");
        let _ = writeln!(out, " * functions in {}.c directly):", m.name);
        for f in &skipped {
            let _ = writeln!(out, " *   {}", f.name);
        }
        let _ = writeln!(out, " */");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "#endif /* {guard} */");
    Some(out)
}
