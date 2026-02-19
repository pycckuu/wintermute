//! Docker executor security invariant tests.

use std::fs;
use std::path::PathBuf;

fn docker_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/executor/docker.rs");
    fs::read_to_string(&path).expect("docker source should load")
}

#[test]
fn docker_container_config_disables_network() {
    let source = docker_source();
    assert!(source.contains("network_mode: Some(\"none\".to_owned())"));
}

#[test]
fn docker_exec_and_container_env_are_empty() {
    let source = docker_source();
    let empty_env_mentions = source.matches("env: Some(Vec::new())").count();
    assert!(empty_env_mentions >= 2);
}

#[test]
fn docker_exec_captures_output() {
    let source = docker_source();
    assert!(source.contains("attach_stdout: Some(true)"));
    assert!(source.contains("attach_stderr: Some(true)"));
}

#[test]
fn docker_output_passes_through_redactor() {
    let source = docker_source();
    assert!(source.contains("self.redactor.redact_result(raw)"));
}

#[test]
fn docker_container_drops_all_capabilities() {
    let source = docker_source();
    assert!(source.contains("cap_drop: Some(vec![\"ALL\".to_owned()])"));
}

#[test]
fn docker_container_sets_security_opt() {
    let source = docker_source();
    assert!(source.contains("no-new-privileges:true"));
}

#[test]
fn docker_container_sets_pids_limit() {
    let source = docker_source();
    assert!(source.contains("pids_limit: Some(256)"));
}

#[test]
fn docker_container_runs_as_non_root_user() {
    let source = docker_source();
    assert!(source.contains("user: Some(\"wintermute\".to_owned())"));
}

#[test]
fn docker_container_has_readonly_rootfs() {
    let source = docker_source();
    assert!(source.contains("readonly_rootfs: Some(true)"));
}
