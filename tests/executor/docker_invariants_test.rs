//! Docker executor security invariant tests.

use std::fs;
use std::path::PathBuf;

fn docker_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/executor/docker.rs");
    let source_result = fs::read_to_string(&path);
    assert!(source_result.is_ok());
    match source_result {
        Ok(source) => source,
        Err(err) => panic!("docker source should load from {}: {err}", path.display()),
    }
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
fn docker_exec_captures_output_and_redacts_it() {
    let source = docker_source();
    assert!(source.contains("attach_stdout: Some(true)"));
    assert!(source.contains("attach_stderr: Some(true)"));
    assert!(source.contains("self.redactor.redact(&stdout_raw)"));
    assert!(source.contains("self.redactor.redact(&stderr_raw)"));
}
