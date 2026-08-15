use sudoc_sdk::Backend;

fn emit_program(files: &[(&str, &str)]) -> Vec<sudoc_sdk::GeneratedFile> {
    let p = sudoc_types::check_program_files(files).expect("checks");
    sudoc_backend_zig::ZigBackend
        .emit_program(&p.modules, true)
        .expect("emit")
}

#[test]
fn sudo_types_file_absent_when_nothing_escapes() {
    let files = emit_program(&[
        ("util", "func one() -> int\n    return 1\n"),
        (
            "main",
            "import util\n\nrecord Home\n    n: int\ntest \"t\"\n    assert util.one() == 1\n",
        ),
    ]);
    assert!(
        files.iter().all(|f| !f.path.contains("sudo_types")),
        "{:?}",
        files.iter().map(|f| f.path.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn list_int_identity_does_not_invent_sudo_types() {
    let files = emit_program(&[
        ("util", "func id<T>(x: T) -> T\n    return x\n"),
        (
            "main",
            "import util\n\ntest \"t\"\n    assert util.id([1, 2]) == [1, 2]\n",
        ),
    ]);
    assert!(
        files.iter().all(|f| !f.path.contains("sudo_types")),
        "{:?}",
        files.iter().map(|f| f.path.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn sudo_types_file_present_when_type_crosses() {
    let files = emit_program(&[
        ("util", "func id<T>(x: T) -> T\n    return x\n"),
        (
            "main",
            "import util\n\nrecord Thing\n    n: int\ntest \"t\"\n    t = util.id(Thing(1))\n    assert t.n == 1\n",
        ),
    ]);
    assert!(
        files.iter().any(|f| f.path == "sudo_types.zig"),
        "{:?}",
        files.iter().map(|f| f.path.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn set_of_tuple_uses_shared_keyapp_without_sudo_types() {
    let files = emit_program(&[
        ("util", "func id<T>(x: T) -> T\n    return x\n"),
        (
            "main",
            "import util\n\ntest \"t\"\n    s: Set<(int, int)> = Set()\n    s.add((1, 2))\n    assert util.id(s).has((1, 2))\n",
        ),
    ]);
    assert!(
        files.iter().all(|f| !f.path.contains("sudo_types")),
        "{:?}",
        files.iter().map(|f| f.path.clone()).collect::<Vec<_>>()
    );
    let joined: String = files.iter().map(|f| f.contents.as_str()).collect();
    assert!(joined.contains("rt.key_tuple2("), "{joined}");
}
