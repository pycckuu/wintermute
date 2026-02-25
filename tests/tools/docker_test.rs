//! Tests for `src/tools/docker.rs` — docker_manage tool definitions and dispatch.

use wintermute::tools::docker::{
    docker_manage_tool_definition, validate_volume_mount, WINTERMUTE_LABEL,
};

// ---------------------------------------------------------------------------
// Tool definition tests
// ---------------------------------------------------------------------------

#[test]
fn docker_manage_definition_has_correct_name() {
    let def = docker_manage_tool_definition();
    assert_eq!(def.name, "docker_manage");
}

#[test]
fn docker_manage_definition_has_description() {
    let def = docker_manage_tool_definition();
    assert!(!def.description.is_empty());
}

#[test]
fn docker_manage_definition_has_valid_schema() {
    let def = docker_manage_tool_definition();
    assert_eq!(
        def.input_schema.get("type").and_then(|v| v.as_str()),
        Some("object")
    );
}

#[test]
fn docker_manage_definition_requires_action() {
    let def = docker_manage_tool_definition();
    let required = def
        .input_schema
        .get("required")
        .and_then(|v| v.as_array())
        .expect("should have required array");
    let required_names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
    assert!(required_names.contains(&"action"));
}

#[test]
fn docker_manage_definition_lists_all_actions() {
    let def = docker_manage_tool_definition();
    let action_enum = def.input_schema["properties"]["action"]["enum"]
        .as_array()
        .expect("action should have enum");
    let actions: Vec<&str> = action_enum.iter().filter_map(|v| v.as_str()).collect();
    assert!(actions.contains(&"run"));
    assert!(actions.contains(&"stop"));
    assert!(actions.contains(&"rm"));
    assert!(actions.contains(&"ps"));
    assert!(actions.contains(&"logs"));
    assert!(actions.contains(&"pull"));
    assert!(actions.contains(&"network_create"));
    assert!(actions.contains(&"network_connect"));
    assert!(actions.contains(&"exec"));
    assert!(actions.contains(&"inspect"));
}

// ---------------------------------------------------------------------------
// Label policy tests
// ---------------------------------------------------------------------------

#[test]
fn wintermute_label_constant_is_correct() {
    assert_eq!(WINTERMUTE_LABEL, "wintermute");
}

// ---------------------------------------------------------------------------
// Volume mount validation tests
// ---------------------------------------------------------------------------

#[test]
fn volume_mount_blocks_docker_socket() {
    let result = validate_volume_mount("/var/run/docker.sock:/var/run/docker.sock");
    assert!(result.is_err());
    let err = result.expect_err("should be blocked").to_string();
    assert!(err.contains("sensitive path"), "error: {err}");
}

#[test]
fn volume_mount_blocks_run_docker_socket() {
    let result = validate_volume_mount("/run/docker.sock:/run/docker.sock");
    assert!(result.is_err());
}

#[test]
fn volume_mount_blocks_etc_prefix() {
    let result = validate_volume_mount("/etc/passwd:/tmp/passwd");
    assert!(result.is_err());
    let err = result.expect_err("should be blocked").to_string();
    assert!(err.contains("protected directory"), "error: {err}");
}

#[test]
fn volume_mount_blocks_proc_prefix() {
    assert!(validate_volume_mount("/proc/1/status:/tmp/status").is_err());
}

#[test]
fn volume_mount_blocks_env_file() {
    let result = validate_volume_mount("/home/user/.env:/app/.env");
    assert!(result.is_err());
    let err = result.expect_err("should be blocked").to_string();
    assert!(err.contains("secrets"), "error: {err}");
}

#[test]
fn volume_mount_blocks_config_toml() {
    assert!(validate_volume_mount("/home/user/.wintermute/config.toml:/cfg").is_err());
}

#[test]
fn volume_mount_allows_safe_path() {
    assert!(validate_volume_mount("/data/app:/app/data").is_ok());
}

#[test]
fn volume_mount_allows_workspace_path() {
    assert!(validate_volume_mount("/home/user/workspace:/workspace").is_ok());
}
