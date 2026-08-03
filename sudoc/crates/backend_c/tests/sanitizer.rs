//! Sanitizer instrumentation: `test_recipe` respects `SUDOC_NO_SANITIZE`,
//! and when instrumented, the built binary is genuinely ASan+UBSan-linked
//! (spec/lockstep.md §5.2).

use std::sync::Mutex;

use sudoc_sdk::Backend;

/// `SUDOC_NO_SANITIZE` is process-global, but `sanitize_status()` (and
/// `test_recipe()`, which calls it) reads it live. The opt-out test below
/// mutates that var while the instrumentation test reads it, and the harness
/// runs `#[test]`s on parallel threads — so without serialization the reader
/// can observe the writer's transient `SUDOC_NO_SANITIZE=1` and see a spurious
/// `DisabledOptOut` (a real flake we hit in CI). Both tests hold this lock for
/// their full body so the opt-out mutation is never visible cross-test; it also
/// keeps the `set_var`/`remove_var` pair off any concurrent reader.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// CI sets `SUDOC_REQUIRE_SANITIZE=1` (via the Bazel test env) so a probe that
/// unexpectedly reports "unsupported" fails LOUD instead of silently skipping —
/// the exact hole that let the C sanitizer gate go dark under Bazel.
fn require_sanitize() -> bool {
    std::env::var("SUDOC_REQUIRE_SANITIZE").as_deref() == Ok("1")
}

#[test]
fn sanitize_recipe_respects_env_opt_out_and_support() {
    // Serialize against the instrumentation test: this body mutates the
    // process-global SUDOC_NO_SANITIZE, which that test reads. Ignore poison —
    // a panic in the other test shouldn't turn this into a lock-panic that
    // masks the original failure.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let backend = sudoc_backend_c::CBackend;

    std::env::set_var("SUDOC_NO_SANITIZE", "1");
    assert_eq!(
        sudoc_backend_c::sanitize_status(),
        sudoc_backend_c::SanitizeStatus::DisabledOptOut
    );
    let recipe = backend.test_recipe("entry");
    assert!(
        !recipe.build[0].iter().any(|a| a.contains("-fsanitize")),
        "opt-out must produce an uninstrumented build: {:?}",
        recipe.build
    );
    assert_eq!(recipe.run, vec!["./sudo_tests".to_string()]);
    std::env::remove_var("SUDOC_NO_SANITIZE");

    // Without the opt-out, on a compiler that supports it (verified
    // separately by the integration test below), the recipe is instrumented.
    if sudoc_backend_c::sanitize_status() == sudoc_backend_c::SanitizeStatus::Enabled {
        let recipe = backend.test_recipe("entry");
        assert!(
            recipe.build[0].iter().any(|a| a == "-fsanitize=address,undefined"),
            "expected instrumentation: {:?}",
            recipe.build
        );
        assert_eq!(recipe.run[0], "env");
        assert!(recipe.run[1].starts_with("ASAN_OPTIONS="));
        assert_eq!(recipe.run[2], "./sudo_tests");
    } else if require_sanitize() {
        panic!(
            "SUDOC_REQUIRE_SANITIZE=1 but sanitize_status()={:?}: the cc probe reported \
             no -fsanitize support, which would silently disable the C sanitizer gate",
            sudoc_backend_c::sanitize_status()
        );
    } else {
        eprintln!(
            "skipping enabled-path assertion: cc here does not support -fsanitize=address,undefined"
        );
    }
}

#[test]
fn conformance_module_c_artifact_is_instrumented() {
    // Held for the whole body: the opt-out test transiently sets
    // SUDOC_NO_SANITIZE, and both `sanitize_status()` and `test_recipe()`
    // (called below) read it live. Without this we can sample it mid-mutation.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if std::env::var("SUDOC_NO_SANITIZE").as_deref() == Ok("1") {
        eprintln!("skipping: SUDOC_NO_SANITIZE=1 in this environment");
        return;
    }
    if sudoc_backend_c::sanitize_status() != sudoc_backend_c::SanitizeStatus::Enabled {
        if require_sanitize() {
            panic!(
                "SUDOC_REQUIRE_SANITIZE=1 but sanitize_status()={:?}: refusing to skip the \
                 instrumented-artifact check",
                sudoc_backend_c::sanitize_status()
            );
        }
        eprintln!("skipping: cc here does not support -fsanitize=address,undefined");
        return;
    }
    // Runtime CARGO_MANIFEST_DIR so arithmetic.sudo resolves under Bazel (staged
    // as data) and cargo alike.
    let src_path =
        std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
            .join("../../../conformance/semantics/arithmetic.sudo");
    let src = std::fs::read_to_string(&src_path).expect("read arithmetic.sudo");
    let ir = sudoc_types::check_source(&src, "arithmetic").expect("checks");

    let dir = std::env::temp_dir().join(format!("sudoc-c-sanitize-it-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("arithmetic.c"), sudoc_backend_c::emit(&ir, true)).unwrap();
    std::fs::write(dir.join(sudoc_backend_c::RUNTIME_H_FILE), sudoc_backend_c::RUNTIME_H).unwrap();
    std::fs::write(dir.join(sudoc_backend_c::RUNTIME_C_FILE), sudoc_backend_c::RUNTIME_C).unwrap();

    let backend = sudoc_backend_c::CBackend;
    let recipe = backend.test_recipe("arithmetic");
    assert!(
        recipe.build[0].iter().any(|a| a == "-fsanitize=address,undefined"),
        "recipe not instrumented: {:?}",
        recipe.build
    );
    for step in &recipe.build {
        let out = std::process::Command::new(&step[0])
            .args(&step[1..])
            .current_dir(&dir)
            .output()
            .expect("build step runs");
        assert!(
            out.status.success(),
            "build failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Robust check #1: the binary carries ASan runtime symbols.
    let bin = dir.join("sudo_tests");
    if let Ok(nm) = std::process::Command::new("nm").arg(&bin).output() {
        let syms = String::from_utf8_lossy(&nm.stdout);
        assert!(
            syms.contains("asan"),
            "expected __asan* symbols in the built binary; nm output had none"
        );
    } else {
        eprintln!("nm unavailable, skipping symbol check");
    }

    // Robust check #2: running with ASAN_OPTIONS=verbosity=1 prints the ASan
    // init banner to stderr, independent of symbol-naming details. Invoke
    // the binary directly (not via recipe.run) so this env var isn't
    // shadowed by the recipe's own baked `env ASAN_OPTIONS=...` wrapper.
    let out = std::process::Command::new(&bin)
        .env("ASAN_OPTIONS", "verbosity=1")
        .current_dir(&dir)
        .output()
        .expect("run step runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("AddressSanitizer"),
        "expected an ASan banner on stderr with verbosity=1; got:\n{stderr}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
