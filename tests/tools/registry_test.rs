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
    let schema = match tool {
        Some(schema) => schema,
        None => panic!("test_tool should be found"),
    };
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
fn registry_ranked_definitions_orders_by_recency() {
    let (_dir, path) = setup_temp_dir_with_tools();
    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");

    registry.record_usage("another_tool");
    std::thread::sleep(std::time::Duration::from_millis(5));
    registry.record_usage("test_tool");

    let defs = registry.ranked_definitions(2, None);
    assert_eq!(defs.len(), 2);
    assert_eq!(
        defs[0].name, "test_tool",
        "most recently used should be first"
    );
}

#[test]
fn registry_ranked_definitions_respects_query_relevance() {
    let (_dir, path) = setup_temp_dir_with_tools();
    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");

    let defs = registry.ranked_definitions(2, Some("test another"));
    assert_eq!(defs.len(), 2);
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"test_tool"));
    assert!(names.contains(&"another_tool"));
}

#[test]
fn registry_ranked_definitions_uses_recency_as_query_tiebreaker() {
    let (_dir, path) = setup_temp_dir_with_tools();
    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");

    // Both descriptions match "tool", so recency should break the tie.
    registry.record_usage("test_tool");
    std::thread::sleep(std::time::Duration::from_millis(5));
    registry.record_usage("another_tool");

    let defs = registry.ranked_definitions(1, Some("tool"));
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].name, "another_tool");
}

#[test]
fn registry_ranked_definitions_respects_max_count() {
    let (_dir, path) = setup_temp_dir_with_tools();
    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");

    let defs = registry.ranked_definitions(1, None);
    assert_eq!(defs.len(), 1);
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

#[tokio::test]
async fn registry_record_execution_updates_meta() {
    let (_dir, path) = setup_temp_dir_with_tools();
    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");

    // Initially no _meta.
    let before = registry.get("test_tool").expect("should exist");
    assert!(before.meta.is_none());

    // Record a successful execution.
    registry.record_execution("test_tool", true, 150, None);

    let after = registry.get("test_tool").expect("should exist");
    let meta = after.meta.expect("_meta should exist after recording");
    assert_eq!(meta.invocations, 1);
    assert!((meta.success_rate - 1.0).abs() < f64::EPSILON);
    assert_eq!(meta.avg_duration_ms, 150);
    assert!(meta.last_used.is_some());
    assert!(meta.last_error.is_none());
}

#[tokio::test]
async fn registry_record_execution_tracks_failures() {
    let (_dir, path) = setup_temp_dir_with_tools();
    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");

    registry.record_execution("test_tool", true, 100, None);
    registry.record_execution("test_tool", false, 200, Some("timeout"));

    let tool = registry.get("test_tool").expect("should exist");
    let meta = tool.meta.expect("_meta should exist");
    assert_eq!(meta.invocations, 2);
    assert!((meta.success_rate - 0.5).abs() < f64::EPSILON);
    assert_eq!(meta.last_error, Some("timeout".to_owned()));
}

#[tokio::test]
async fn registry_all_schemas_returns_meta() {
    let (_dir, path) = setup_temp_dir_with_tools();
    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");

    registry.record_execution("test_tool", true, 100, None);

    let schemas = registry.all_schemas();
    let with_meta = schemas
        .iter()
        .find(|s| s.name == "test_tool")
        .expect("found");
    assert!(
        with_meta.meta.is_some(),
        "_meta should be present in all_schemas"
    );
}

#[tokio::test]
async fn registry_meta_not_in_tool_definition_output() {
    let (_dir, path) = setup_temp_dir_with_tools();
    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");

    registry.record_execution("test_tool", true, 100, None);

    let defs = registry.all_definitions();
    let def = defs.iter().find(|d| d.name == "test_tool").expect("found");
    // ToolDefinition only has name, description, input_schema — no _meta field.
    let schema_str = serde_json::to_string(&def.input_schema).expect("serialize");
    assert!(
        !schema_str.contains("_meta"),
        "ToolDefinition should not include _meta"
    );
}

#[test]
fn registry_meta_preserved_across_reload() {
    let dir = TempDir::new().expect("should create temp dir");
    let path = dir.path().to_path_buf();

    let schema = json!({
        "name": "meta_tool",
        "description": "Tool with meta",
        "parameters": { "type": "object" },
        "_meta": {
            "created_at": "2025-01-01T00:00:00Z",
            "last_used": null,
            "invocations": 5,
            "success_rate": 0.8,
            "avg_duration_ms": 200,
            "last_error": null,
            "version": 2
        }
    });
    std::fs::write(
        path.join("meta_tool.json"),
        serde_json::to_string_pretty(&schema).expect("serialize"),
    )
    .expect("write");

    let registry =
        DynamicToolRegistry::new_without_watcher(path.clone()).expect("registry should initialise");

    let tool = registry.get("meta_tool").expect("should exist");
    let meta = tool.meta.expect("_meta should be loaded");
    assert_eq!(meta.invocations, 5);
    assert!((meta.success_rate - 0.8).abs() < f64::EPSILON);
    assert_eq!(meta.version, 2);

    // Reload and verify _meta is preserved.
    registry.reload_all().expect("reload should succeed");
    let reloaded = registry.get("meta_tool").expect("should exist");
    let meta2 = reloaded.meta.expect("_meta should survive reload");
    assert_eq!(meta2.invocations, 5);
}

#[test]
fn registry_rejects_invalid_timeout_values() {
    let dir = TempDir::new().expect("should create temp dir");
    let path = dir.path().to_path_buf();

    let schema = json!({
        "name": "bad_timeout_tool",
        "description": "Tool with invalid timeout",
        "parameters": { "type": "object" },
        "timeout_secs": 999999
    });
    std::fs::write(
        path.join("bad_timeout_tool.json"),
        serde_json::to_string_pretty(&schema).expect("serialize"),
    )
    .expect("write");

    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");
    assert_eq!(
        registry.count(),
        0,
        "invalid timeout schema should be skipped"
    );
}
