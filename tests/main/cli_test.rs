//! CLI contract tests.

use std::fs;
use std::path::PathBuf;

fn main_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs");
    let source_result = fs::read_to_string(&path);
    assert!(source_result.is_ok());
    match source_result {
        Ok(source) => source,
        Err(err) => panic!("main source should load from {}: {err}", path.display()),
    }
}

#[test]
fn main_defines_primary_subcommands() {
    let source = main_source();
    assert!(source.contains("Init"));
    assert!(source.contains("Start"));
    assert!(source.contains("Status"));
    assert!(source.contains("Reset"));
    assert!(source.contains("Backup"));
}
