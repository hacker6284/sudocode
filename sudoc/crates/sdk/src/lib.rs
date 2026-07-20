//! The sudo backend SDK: the contract every target language implements.
//!
//! A backend is four things (lockstep.md §5.3):
//! 1. a type mapping from sudo types to natural host types,
//! 2. value-semantics copy points that make aliasing unobservable,
//! 3. a trap surface (how the closed set of trap kinds is reported),
//! 4. a test runner that prints the TAP-ish outcome protocol below.
//!
//! Implement [`Backend`], register it (see the harness crate's registry),
//! and validate with `sudoc conformance --target <name>` — your backend must
//! agree with the reference backends on the whole conformance corpus.
//! See spec/backend-guide.md for the step-by-step handbook.
//!
//! ## The outcome protocol
//!
//! A generated test artifact, when run, prints one line per test, in
//! declaration order, to stdout:
//!
//! ```text
//! ok 1 - test_name
//! not ok 2 - test_name [TrapKind]
//! not ok 3 - test_name [TrapKind: optional detail]
//! ```
//!
//! and exits nonzero iff any test failed. Test function names come from
//! `sudoc_ir::names::test_fn_names` — never invent your own scheme, the
//! harness aligns outcomes across targets by these names.

use std::path::Path;

use sudoc_ir::IrModule;

/// A file the backend wants written into the output directory.
#[derive(Debug, Clone)]
pub struct GeneratedFile {
    /// Path relative to the output directory (no leading separators).
    pub path: String,
    pub contents: String,
}

/// How to build and run a generated test artifact. All commands run with
/// the output directory as the working directory.
#[derive(Debug, Clone)]
pub struct TestRecipe {
    /// Build steps (compilers, bundlers); may be empty for interpreted
    /// targets. Each is an argv vector.
    pub build: Vec<Vec<String>>,
    /// The command whose stdout is the outcome protocol.
    pub run: Vec<String>,
}

/// One target language.
pub trait Backend {
    /// Short CLI name (`py`, `c`, `js`, ...).
    fn name(&self) -> &str;

    /// Emit source for a checked program (dependencies first, entry module
    /// last — the order `sudoc_types::check_program` produces). When
    /// `with_tests` is set, the entry module's `test` blocks must become a
    /// runnable artifact per the outcome protocol.
    ///
    /// Emission can fail (compile-time-ish errors from an external backend
    /// process). The error is a human-readable message; for external backends
    /// it includes captured stderr.
    fn emit_program(
        &self,
        modules: &[IrModule],
        with_tests: bool,
    ) -> Result<Vec<GeneratedFile>, String>;

    /// Support files shipped alongside every generated program (runtimes).
    fn runtime_files(&self) -> Vec<GeneratedFile>;

    /// How to build and run the test artifact for `entry` (a module name).
    fn test_recipe(&self, entry: &str) -> TestRecipe;
}

/// Write a backend's output for a program into `dir`.
///
/// Propagates emit failures and maps I/O errors to a plain readable string.
pub fn write_output(
    backend: &dyn Backend,
    modules: &[IrModule],
    with_tests: bool,
    dir: &Path,
) -> Result<(), String> {
    let files = backend.emit_program(modules, with_tests)?;
    for f in files.into_iter().chain(backend.runtime_files()) {
        let path = dir.join(&f.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        std::fs::write(&path, f.contents).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}
