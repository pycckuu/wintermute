//! Security invariant regression checks.

use std::path::{Path, PathBuf};

use wintermute::config::runtime_paths;
use wintermute::telegram::input_guard::{scan_message, GuardAction};

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_rust_files(&path, out)?;
        } else if metadata.is_file() && path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

#[test]
fn no_host_process_command_apis_in_src() -> Result<(), Box<dyn std::error::Error>> {
    let src_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut rust_files = Vec::new();
    collect_rust_files(&src_dir, &mut rust_files)?;

    let forbidden = ["std::process::Command", "tokio::process::Command"];
    for path in rust_files {
        let content = std::fs::read_to_string(&path)?;
        for pattern in forbidden {
            assert!(
                !content.contains(pattern),
                "forbidden host process-command API '{pattern}' found in {}",
                path.display()
            );
        }
    }
    Ok(())
}

#[test]
fn security_invariant_6_inbound_credentials_are_scanned() {
    let action = scan_message(
        "Please use token SECRET_TOKEN_123 for this request and continue.",
        &[String::from("SECRET_TOKEN_123")],
    );
    assert!(matches!(action, GuardAction::Redacted(_)));

    let blocked = scan_message("SECRET_TOKEN_123", &[String::from("SECRET_TOKEN_123")]);
    assert!(matches!(blocked, GuardAction::Blocked));
}

#[test]
fn security_invariant_8_config_split_paths_are_enforced() -> Result<(), Box<dyn std::error::Error>>
{
    let paths = runtime_paths()?;
    assert!(
        !paths.config_toml.starts_with(&paths.scripts_dir),
        "config.toml must stay outside agent-writable scripts directory"
    );
    assert!(
        paths.agent_toml != paths.config_toml,
        "agent.toml and config.toml must be separate files"
    );
    Ok(())
}

#[test]
fn security_invariant_5_budget_check_precedes_provider_call(
) -> Result<(), Box<dyn std::error::Error>> {
    let loop_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/agent/loop.rs");
    let content = std::fs::read_to_string(loop_src)?;
    let budget_idx = content
        .find("check_budget")
        .ok_or("missing check_budget call in agent loop")?;
    let provider_idx = content
        .find("provider.complete")
        .ok_or("missing provider.complete call in agent loop")?;
    assert!(
        budget_idx < provider_idx,
        "budget check must happen before provider call in agent loop"
    );
    Ok(())
}

#[test]
fn security_invariant_2_container_env_only_proxy_vars() -> Result<(), Box<dyn std::error::Error>> {
    let docker_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/executor/docker.rs");
    let content = std::fs::read_to_string(docker_src)?;
    // Container env must contain only proxy variables (or be empty in test mode).
    assert!(
        content.contains("HTTP_PROXY=") && content.contains("HTTPS_PROXY="),
        "docker container env must set HTTP_PROXY and HTTPS_PROXY for egress proxy"
    );
    // Exec env must remain empty (no secrets leak into executed commands).
    assert!(
        content.contains("env: Some(Vec::new())"),
        "docker exec options must keep env empty (no secret leakage)"
    );
    Ok(())
}

#[test]
fn security_invariant_3_container_network_via_proxy() -> Result<(), Box<dyn std::error::Error>> {
    let docker_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/executor/docker.rs");
    let content = std::fs::read_to_string(docker_src)?;
    // The sandbox joins the egress proxy network (or falls back to none for tests).
    assert!(
        content.contains("network_name") && content.contains("proxy_address"),
        "docker sandbox must accept network and proxy configuration for egress proxy"
    );
    // The egress module must define the proxy network and enforce domain allowlist.
    let egress_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/executor/egress.rs");
    let egress_content = std::fs::read_to_string(egress_src)?;
    assert!(
        egress_content.contains("wintermute-net")
            && egress_content.contains("http_access deny all"),
        "egress proxy must use wintermute-net network and deny-all default policy"
    );
    Ok(())
}

#[test]
fn security_invariant_4_browser_navigate_uses_domain_policy(
) -> Result<(), Box<dyn std::error::Error>> {
    let policy_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/agent/policy.rs");
    let content = std::fs::read_to_string(policy_src)?;
    assert!(
        content.contains("fn check_browser_policy")
            && content.contains("if action == \"navigate\"")
            && content.contains("check_domain_policy(input, ctx, is_domain_trusted)"),
        "browser navigate must flow through domain policy evaluation"
    );
    Ok(())
}

#[test]
fn security_invariant_7_redactor_is_tool_output_chokepoint(
) -> Result<(), Box<dyn std::error::Error>> {
    let tools_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/tools/mod.rs");
    let content = std::fs::read_to_string(tools_src)?;
    assert!(
        content.contains("self.redactor.redact(&raw_result.content)"),
        "tool router must redact all tool output before returning"
    );
    Ok(())
}
