//! Tests for `src/tools/registry.rs` — dynamic tool registry.

use std::path::PathBuf;

use serde_json::json;
use tempfile::TempDir;

use wintermute::tools::registry::DynamicToolRegistry;

/// Create a temp directory with some tool JSON files.
fn setup_temp_dir_with_tools() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("should create temp dir");
    let path = dir.path().to_path_buf();

    let schema = json!({
        "name": "test_tool",
        "description": "A test tool",
        "parameters": {
            "type": "object",
            "properties": {
                "input": { "type": "string" }
            }
        },
        "timeout_secs": 60
    });
    std::fs::write(
        path.join("test_tool.json"),
        serde_json::to_string_pretty(&schema).expect("serialize"),
    )
    .expect("write");

    let schema2 = json!({
        "name": "another_tool",
        "description": "Another tool",
        "parameters": {
            "type": "object"
        }
    });
    std::fs::write(
        path.join("another_tool.json"),
        serde_json::to_string_pretty(&schema2).expect("serialize"),
    )
    .expect("write");

    (dir, path)
}

#[test]
fn registry_loads_json_files_from_directory() {
    let (_dir, path) = setup_temp_dir_with_tools();
    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");

    assert_eq!(registry.count(), 2, "should load 2 tools");
}

#[test]
fn registry_get_returns_existing_tool() {
    let (_dir, path) = setup_temp_dir_with_tools();
    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");

    let tool = registry.get("test_tool");
    assert!(tool.is_some(), "test_tool should be found");
    let schema = tool.expect("just checked");
    assert_eq!(schema.name, "test_tool");
    assert_eq!(schema.description, "A test tool");
    assert_eq!(schema.timeout_secs, 60);
}

#[test]
fn registry_get_returns_none_for_missing_tool() {
    let (_dir, path) = setup_temp_dir_with_tools();
    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");

    let tool = registry.get("nonexistent_tool");
    assert!(tool.is_none(), "nonexistent tool should not be found");
}

#[test]
fn registry_count_reflects_loaded_tools() {
    let dir = TempDir::new().expect("should create temp dir");
    let path = dir.path().to_path_buf();

    // No tools initially.
    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");
    assert_eq!(registry.count(), 0, "empty dir should have 0 tools");
}

#[test]
fn registry_reload_all_picks_up_new_files() {
    let dir = TempDir::new().expect("should create temp dir");
    let path = dir.path().to_path_buf();

    let registry =
        DynamicToolRegistry::new_without_watcher(path.clone()).expect("registry should initialise");
    assert_eq!(registry.count(), 0);

    // Write a new tool file.
    let schema = json!({
        "name": "new_tool",
        "description": "Newly added tool",
        "parameters": { "type": "object" }
    });
    std::fs::write(
        path.join("new_tool.json"),
        serde_json::to_string_pretty(&schema).expect("serialize"),
    )
    .expect("write");

    registry.reload_all().expect("reload should succeed");
    assert_eq!(registry.count(), 1, "should now have 1 tool after reload");
    assert!(registry.get("new_tool").is_some());
}

#[test]
fn registry_skips_invalid_json_files() {
    let dir = TempDir::new().expect("should create temp dir");
    let path = dir.path().to_path_buf();

    // Write a valid tool.
    let valid = json!({
        "name": "valid_tool",
        "description": "Valid",
        "parameters": { "type": "object" }
    });
    std::fs::write(
        path.join("valid_tool.json"),
        serde_json::to_string_pretty(&valid).expect("serialize"),
    )
    .expect("write");

    // Write an invalid JSON file.
    std::fs::write(path.join("broken.json"), "this is not valid json {{{").expect("write");

    // Should load without panicking.
    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");
    assert_eq!(
        registry.count(),
        1,
        "should load only the valid tool, skipping the broken one"
    );
    assert!(registry.get("valid_tool").is_some());
}

#[test]
fn registry_all_definitions_converts_to_tool_definitions() {
    let (_dir, path) = setup_temp_dir_with_tools();
    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");

    let defs = registry.all_definitions();
    assert_eq!(defs.len(), 2);

    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"test_tool"));
    assert!(names.contains(&"another_tool"));
}

#[test]
fn registry_reload_tool_updates_specific_tool() {
    let (_dir, path) = setup_temp_dir_with_tools();
    let registry =
        DynamicToolRegistry::new_without_watcher(path.clone()).expect("registry should initialise");

    // Update the test_tool description.
    let updated = json!({
        "name": "test_tool",
        "description": "Updated description",
        "parameters": { "type": "object" },
        "timeout_secs": 30
    });
    std::fs::write(
        path.join("test_tool.json"),
        serde_json::to_string_pretty(&updated).expect("serialize"),
    )
    .expect("write");

    registry
        .reload_tool("test_tool")
        .expect("reload should succeed");

    let tool = registry.get("test_tool").expect("should exist");
    assert_eq!(tool.description, "Updated description");
    assert_eq!(tool.timeout_secs, 30);
}

#[test]
fn registry_handles_nonexistent_directory() {
    let path = PathBuf::from("/tmp/nonexistent_wintermute_test_dir_12345");
    let registry = DynamicToolRegistry::new_without_watcher(path);
    assert!(registry.is_ok(), "should handle nonexistent dir gracefully");
    assert_eq!(registry.expect("just checked").count(), 0);
}

#[test]
fn registry_default_timeout_is_120() {
    let dir = TempDir::new().expect("should create temp dir");
    let path = dir.path().to_path_buf();

    // Schema without timeout_secs field.
    let schema = json!({
        "name": "no_timeout_tool",
        "description": "Tool without timeout",
        "parameters": { "type": "object" }
    });
    std::fs::write(
        path.join("no_timeout_tool.json"),
        serde_json::to_string_pretty(&schema).expect("serialize"),
    )
    .expect("write");

    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");
    let tool = registry.get("no_timeout_tool").expect("should exist");
    assert_eq!(tool.timeout_secs, 120, "default timeout should be 120");
}
