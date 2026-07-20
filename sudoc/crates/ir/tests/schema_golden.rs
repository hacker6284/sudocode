//! Golden for the generated IR wire schema (spec/protocol/ir-schema.json).
//! Regenerate with `BLESS=1 cargo test -p sudoc-ir --test schema_golden`.

use schemars::schema_for;
use sudoc_ir::IrModule;

#[test]
fn ir_schema_golden() {
    let schema_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../spec/protocol/ir-schema.json");
    let schema = schema_for!(Vec<IrModule>);
    let dump = serde_json::to_string_pretty(&schema).expect("schema json");
    let bless = std::env::var("BLESS").is_ok();
    if bless {
        if let Some(parent) = schema_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&schema_path, &dump).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&schema_path).unwrap_or_else(|_| {
        panic!(
            "missing golden {schema_path:?}; run BLESS=1 cargo test -p sudoc-ir to create"
        )
    });
    assert_eq!(
        dump, expected,
        "IR schema drifted — review, then BLESS=1 cargo test -p sudoc-ir to accept"
    );
}
