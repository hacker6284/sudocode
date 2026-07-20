//! Swift backend: typed IR → readable Swift 6 source.
//!
//! Strategy (see notes/friction-swift.md for verification notes):
//! - value types are native (struct/enum/Array/Dictionary/Set COW) — no
//!   defensive-copy or free machinery;
//! - deep structural `==` is synthesized Equatable / container equality;
//! - multi-module programs merge into one translation unit with `module__`
//!   prefixes (see `program::merge`);
//! - Int64 arithmetic goes through checked runtime helpers; floats are bare
//!   IEEE; traps are `SudoTrap: Error` caught by the TAP runner;
//! - `for i = a to b` uses lazy `SudoRange` so continue is safe at Int64 bounds.

mod code_gen;
mod program;
mod types_gen;

use sudoc_ir::IrModule;

/// Shared Swift runtime, written alongside every generated module.
pub const RUNTIME: &str = include_str!("runtime/sudo_rt.swift");
pub const RUNTIME_FILE: &str = "sudo_rt.swift";

/// Emit a self-contained Swift source file for the (already merged) IR.
pub fn emit(module: &IrModule, with_tests: bool) -> String {
    code_gen::emit(module, with_tests)
}

/// Merge multi-module IR then emit one Swift file (deps first, entry last).
pub fn emit_program(modules: &[IrModule], with_tests: bool) -> String {
    emit(&program::merge(modules), with_tests)
}

/// The Swift backend, via the SDK contract.
pub struct SwiftBackend;

impl sudoc_sdk::Backend for SwiftBackend {
    fn name(&self) -> &'static str {
        "swift"
    }

    fn emit_program(
        &self,
        modules: &[IrModule],
        with_tests: bool,
    ) -> Vec<sudoc_sdk::GeneratedFile> {
        let merged = program::merge(modules);
        vec![sudoc_sdk::GeneratedFile {
            path: format!("{}.swift", merged.name),
            contents: emit(&merged, with_tests),
        }]
    }

    fn runtime_files(&self) -> Vec<sudoc_sdk::GeneratedFile> {
        vec![sudoc_sdk::GeneratedFile {
            path: RUNTIME_FILE.into(),
            contents: RUNTIME.into(),
        }]
    }

    fn test_recipe(&self, entry: &str) -> sudoc_sdk::TestRecipe {
        sudoc_sdk::TestRecipe {
            build: vec![vec![
                "swiftc".into(),
                "-parse-as-library".into(),
                "-o".into(),
                "sudo_tests".into(),
                format!("{entry}.swift"),
                RUNTIME_FILE.into(),
            ]],
            run: vec!["./sudo_tests".into()],
        }
    }
}
