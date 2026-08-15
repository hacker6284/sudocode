//! End-to-end sanitizer-recipe gate (Phase 5 audit finding 1).
//!
//! backend_c's own `tests/sanitizer.rs` proves the *library* instruments and
//! that the built binary links ASan. But the DEPLOYED path the decomposed
//! lockstep C run-leaf executes is the recipe JSON emitted by
//! `sudoc emit-recipe --target c` — a SEPARATE process (the `_lockstep_emit`
//! `local` action), with its own host-`cc` `-fsanitize` support probe. If THAT
//! probe transiently fails, emit-recipe silently yields an UNINSTRUMENTED
//! recipe, the C run-leaf compiles without sanitizers, and the gate goes dark
//! with every test still green. This test runs the real binary and asserts the
//! emitted recipe carries `-fsanitize=address,undefined`. On Linux
//! (SUDOC_REQUIRE_SANITIZE=1, set by the Bazel test env) a missing flag is a
//! hard failure — the host cc there always supports it, so absence is a
//! regression, not an environment quirk; off Linux it degrades to a skip.

use std::path::PathBuf;
use std::process::Command;

fn sudoc_bin() -> PathBuf {
    if let Ok(p) = std::env::var("SUDOC_BIN") {
        let p = PathBuf::from(p);
        return if p.is_absolute() {
            p
        } else {
            std::env::current_dir().expect("cwd").join(p)
        };
    }
    match option_env!("CARGO_BIN_EXE_sudoc") {
        Some(p) => PathBuf::from(p),
        None => panic!("set SUDOC_BIN (Bazel) or run under cargo (CARGO_BIN_EXE_sudoc)"),
    }
}

fn arithmetic() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("../../../conformance/semantics/arithmetic.sudo")
}

#[test]
fn c_emit_recipe_carries_sanitizer_flags() {
    let out = Command::new(sudoc_bin())
        .args(["emit-recipe", "--target", "c"])
        .arg(arithmetic())
        .output()
        .expect("run sudoc emit-recipe --target c");
    assert!(
        out.status.success(),
        "emit-recipe --target c failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let recipe: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("emit-recipe output parses as JSON");
    let build = recipe["build"]
        .as_array()
        .expect("recipe.build is an array");
    let instrumented = build.iter().any(|step| {
        step.as_array()
            .map(|args| {
                args.iter()
                    .any(|a| a.as_str() == Some("-fsanitize=address,undefined"))
            })
            .unwrap_or(false)
    });

    let require = std::env::var("SUDOC_REQUIRE_SANITIZE").as_deref() == Ok("1");
    if instrumented {
        // The deployed recipe is instrumented — the gate is live.
    } else if require {
        panic!(
            "SUDOC_REQUIRE_SANITIZE=1 but `sudoc emit-recipe --target c` produced an \
             UNINSTRUMENTED recipe (host-cc -fsanitize probe failed in the emit action?): {}",
            String::from_utf8_lossy(&out.stdout)
        );
    } else {
        eprintln!(
            "skipping: cc here does not support -fsanitize=address,undefined \
             (emit-recipe produced an uninstrumented C recipe)"
        );
    }
}
