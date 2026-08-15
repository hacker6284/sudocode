//! Escape analysis and `sudo_types` synthesis.
//!
//! A nominal decl escapes if any other module references it. The shared set
//! is that escaping set closed under record-field and enum-payload types.
//! Escaping decls move into a synthetic leaf module placed first in `modules`.

use std::collections::{HashMap, HashSet};

use sudoc_ir::mangle::SHARED_TYPES_MODULE;
use sudoc_ir::{IrEnum, IrModule, IrRecord, Ty};

use crate::mangle_check::{walk_module, TySink};
use crate::TypeError;

/// Collect every nominal IR symbol mentioned in `ty`.
fn collect_nominals(ty: &Ty, out: &mut HashSet<String>) {
    match ty {
        Ty::Record(n) | Ty::Enum(n) => {
            out.insert(n.clone());
        }
        Ty::List(e) | Ty::Set(e) | Ty::Option_(e) => collect_nominals(e, out),
        Ty::Map(k, v) | Ty::Result_(k, v) => {
            collect_nominals(k, out);
            collect_nominals(v, out);
        }
        Ty::Tuple(ts) => {
            for t in ts {
                collect_nominals(t, out);
            }
        }
        Ty::Func { params, ret } => {
            for p in params {
                collect_nominals(p, out);
            }
            if let Some(r) = ret {
                collect_nominals(r, out);
            }
        }
        _ => {}
    }
}

struct NominalRefs {
    names: HashSet<String>,
}

impl TySink for NominalRefs {
    fn ty(&mut self, t: &Ty) -> Result<(), TypeError> {
        collect_nominals(t, &mut self.names);
        Ok(())
    }
}

/// Move escaping nominal decls into a synthetic `sudo_types` module (first
/// in `modules`) and prepend that import on every module that references a
/// shared type. No-op when nothing escapes — existing programs stay
/// byte-identical.
pub(crate) fn hoist_escaping_types(modules: &mut Vec<IrModule>) {
    if modules.is_empty() {
        return;
    }

    let mut home: HashMap<String, usize> = HashMap::new();
    for (i, m) in modules.iter().enumerate() {
        for r in &m.records {
            home.insert(r.name.clone(), i);
        }
        for e in &m.enums {
            home.insert(e.name.clone(), i);
        }
    }

    let mut refs: Vec<HashSet<String>> = Vec::with_capacity(modules.len());
    for m in modules.iter() {
        let mut obs = NominalRefs {
            names: HashSet::new(),
        };
        // Walker is infallible for this sink.
        walk_module(m, &mut obs).expect("NominalRefs::ty is infallible");
        refs.push(obs.names);
    }

    let mut escaping: HashSet<String> = HashSet::new();
    for (i, names) in refs.iter().enumerate() {
        for n in names {
            if let Some(&h) = home.get(n) {
                if h != i {
                    escaping.insert(n.clone());
                }
            }
        }
    }
    if escaping.is_empty() {
        return;
    }

    // Close under record-field and enum-payload types.
    let field_nominals = |modules: &[IrModule], name: &str| -> HashSet<String> {
        let mut out = HashSet::new();
        for m in modules {
            if let Some(r) = m.record(name) {
                for f in &r.fields {
                    collect_nominals(&f.ty, &mut out);
                }
            }
            if let Some(e) = m.enum_(name) {
                for v in &e.variants {
                    for f in &v.fields {
                        collect_nominals(&f.ty, &mut out);
                    }
                }
            }
        }
        out
    };
    let mut shared = escaping;
    let mut stack: Vec<String> = shared.iter().cloned().collect();
    while let Some(name) = stack.pop() {
        for dep in field_nominals(modules, &name) {
            if home.contains_key(&dep) && shared.insert(dep.clone()) {
                stack.push(dep);
            }
        }
    }

    let mut recs: Vec<IrRecord> = Vec::new();
    let mut enums: Vec<IrEnum> = Vec::new();
    for m in modules.iter_mut() {
        let kept_r: Vec<IrRecord> = std::mem::take(&mut m.records)
            .into_iter()
            .filter_map(|r| {
                if shared.contains(&r.name) {
                    recs.push(r);
                    None
                } else {
                    Some(r)
                }
            })
            .collect();
        m.records = kept_r;
        let kept_e: Vec<IrEnum> = std::mem::take(&mut m.enums)
            .into_iter()
            .filter_map(|e| {
                if shared.contains(&e.name) {
                    enums.push(e);
                    None
                } else {
                    Some(e)
                }
            })
            .collect();
        m.enums = kept_e;
    }

    recs.sort_by(|a, b| a.name.cmp(&b.name));
    recs = sudoc_ir::topo_sort_records(recs);
    enums.sort_by(|a, b| a.name.cmp(&b.name));

    for (i, m) in modules.iter_mut().enumerate() {
        let uses = refs[i].iter().any(|n| shared.contains(n));
        if uses && !m.imports.iter().any(|s| s == SHARED_TYPES_MODULE) {
            m.imports.insert(0, SHARED_TYPES_MODULE.to_string());
        }
    }

    modules.insert(
        0,
        IrModule {
            name: SHARED_TYPES_MODULE.to_string(),
            imports: Vec::new(),
            records: recs,
            enums,
            consts: Vec::new(),
            funcs: Vec::new(),
            tests: Vec::new(),
        },
    );
}
