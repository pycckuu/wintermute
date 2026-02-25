//! Docker executor security invariant tests.

use std::fs;
use std::path::PathBuf;

fn docker_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/executor/docker.rs");
    fs::read_to_string(&path).expect("docker source should load")
}

fn egress_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/executor/egress.rs");
    fs::read_to_string(&path).expect("egress source should load")
}

#[test]
fn docker_container_network_uses_proxy_or_none() {
    let source = docker_source();
    // Network mode is parameterized: proxy network when available, "none" as fallback.
    assert!(source.contains("network_name"));
    assert!(source.contains(".unwrap_or_else(|| \"none\".to_owned())"));
}

#[test]
fn docker_container_env_sets_proxy_vars() {
    let source = docker_source();
    // Container env sets proxy variables for egress routing.
    assert!(source.contains("HTTP_PROXY="));
    assert!(source.contains("HTTPS_PROXY="));
    assert!(source.contains("http_proxy="));
    assert!(source.contains("https_proxy="));
}

#[test]
fn docker_exec_env_is_empty() {
    let source = docker_source();
    // Exec env must remain empty (inherits container env, no extra secrets).
    assert!(source.contains("env: Some(Vec::new())"));
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

#[test]
fn sandbox_image_pulled_before_container_creation() {
    let source = docker_source();
    // Search within create_container function body, not the whole file.
    let fn_start = source
        .find("async fn create_container")
        .expect("create_container function must exist");
    let body = &source[fn_start..];
    let pull_pos = body
        .find("ensure_image")
        .expect("create_container must call ensure_image");
    let create_pos = body
        .find("create_container(options")
        .expect("must call docker.create_container");
    assert!(
        pull_pos < create_pos,
        "ensure_image must be called before docker.create_container"
    );
}

#[test]
fn egress_image_pulled_before_container_creation() {
    let source = egress_source();
    // Search within create_squid_container function body, not the whole file.
    let fn_start = source
        .find("async fn create_squid_container")
        .expect("create_squid_container function must exist");
    let body = &source[fn_start..];
    let pull_pos = body
        .find("ensure_image")
        .expect("create_squid_container must call ensure_image");
    let create_pos = body
        .find("create_container(options")
        .expect("must call docker.create_container");
    assert!(
        pull_pos < create_pos,
        "ensure_image must be called before docker.create_container in egress"
    );
}

#[test]
fn create_dockerfile_tar_produces_valid_archive() {
    let content = b"FROM alpine:latest\nRUN echo hello\n";
    let tar = wintermute::executor::create_dockerfile_tar(content);

    // Archive = 512 header + ceil_512(content) + 1024 end blocks.
    let data_blocks = if content.len().is_multiple_of(512) {
        content.len()
    } else {
        (content.len() / 512 + 1) * 512
    };
    let expected_len = 512 + data_blocks + 1024;
    assert_eq!(tar.len(), expected_len, "tar archive size mismatch");

    // Entry name starts with "Dockerfile".
    assert_eq!(&tar[..10], b"Dockerfile");

    // File content follows header at offset 512.
    assert_eq!(&tar[512..512 + content.len()], content.as_slice());

    // Padding bytes between content end and next block are zero.
    for &b in &tar[512 + content.len()..512 + data_blocks] {
        assert_eq!(b, 0, "padding byte must be zero");
    }

    // Last 1024 bytes are all zeros (end-of-archive marker).
    for &b in &tar[tar.len() - 1024..] {
        assert_eq!(b, 0, "end-of-archive byte must be zero");
    }

    // Type flag at offset 156 is '0' (regular file).
    assert_eq!(tar[156], b'0');
}
