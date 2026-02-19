//! Tests for `telegram::commands` slash command handlers.

use wintermute::telegram::commands;

#[test]
fn help_returns_html_with_command_list() {
    let result = commands::handle_help();
    assert!(result.contains("<b>Available commands:</b>"));
    assert!(result.contains("/help"));
    assert!(result.contains("/status"));
    assert!(result.contains("/budget"));
    assert!(result.contains("/memory"));
    assert!(result.contains("/tools"));
    assert!(result.contains("/sandbox"));
    assert!(result.contains("/backup"));
}

#[test]
fn budget_returns_formatted_budget_string() {
    let result = commands::handle_budget(100, 500, 50_000, 5_000_000);
    assert!(result.contains("Budget"));
    assert!(result.contains("100"));
    assert!(result.contains("500"));
    assert!(result.contains("50000"));
    assert!(result.contains("5000000"));
}

#[test]
fn memory_undo_returns_placeholder() {
    let result = commands::handle_memory_undo();
    assert!(result.contains("not yet available"));
}

#[test]
fn memory_pending_returns_placeholder() {
    let result = commands::handle_memory_pending();
    assert!(result.contains("not yet active"));
}

#[test]
fn backup_trigger_returns_placeholder() {
    let result = commands::handle_backup_trigger();
    assert!(result.contains("not yet automated"));
}

#[test]
fn tools_with_empty_registry_returns_no_tools_message() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let registry = wintermute::tools::registry::DynamicToolRegistry::new_without_watcher(
        temp_dir.path().to_path_buf(),
    )
    .expect("failed to create registry");
    let result = commands::handle_tools(&registry);
    assert!(result.contains("No dynamic tools registered"));
}

#[test]
fn tools_detail_not_found() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let registry = wintermute::tools::registry::DynamicToolRegistry::new_without_watcher(
        temp_dir.path().to_path_buf(),
    )
    .expect("failed to create registry");
    let result = commands::handle_tools_detail(&registry, "nonexistent");
    assert!(result.contains("not found"));
}

#[test]
fn tools_with_registered_tool_shows_it() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let tool_json = serde_json::json!({
        "name": "my_tool",
        "description": "A test tool",
        "parameters": { "type": "object", "properties": {} }
    });
    let tool_path = temp_dir.path().join("my_tool.json");
    std::fs::write(&tool_path, serde_json::to_string(&tool_json).expect("json")).expect("write");

    let registry = wintermute::tools::registry::DynamicToolRegistry::new_without_watcher(
        temp_dir.path().to_path_buf(),
    )
    .expect("failed to create registry");

    let list_result = commands::handle_tools(&registry);
    assert!(list_result.contains("my_tool"));
    assert!(list_result.contains("A test tool"));

    let detail_result = commands::handle_tools_detail(&registry, "my_tool");
    assert!(detail_result.contains("my_tool"));
    assert!(detail_result.contains("A test tool"));
    assert!(detail_result.contains("120s")); // default timeout
}
