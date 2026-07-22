//! Sanitizer instrumentation: `test_recipe` respects `SUDOC_NO_SANITIZE`,
//! and when instrumented, the built binary is genuinely ASan+UBSan-linked
//! (spec/lockstep.md §5.2).

use sudoc_sdk::Backend;

#[test]
fn sanitize_recipe_respects_env_opt_out_and_support() {
    // The only test in this crate touching SUDOC_NO_SANITIZE — no cross-test
    // race from cargo test's parallel test threads.
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
    } else {
        eprintln!(
            "skipping enabled-path assertion: cc here does not support -fsanitize=address,undefined"
        );
    }
}

#[test]
fn conformance_module_c_artifact_is_instrumented() {
    if std::env::var("SUDOC_NO_SANITIZE").as_deref() == Ok("1") {
        eprintln!("skipping: SUDOC_NO_SANITIZE=1 in this environment");
        return;
    }
    if sudoc_backend_c::sanitize_status() != sudoc_backend_c::SanitizeStatus::Enabled {
        eprintln!("skipping: cc here does not support -fsanitize=address,undefined");
        return;
    }
    let src_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
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
