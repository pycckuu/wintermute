//! Tests for the System Identity Document (SID) generator.

use std::time::Duration;

use wintermute::agent::identity::{
    format_uptime, load_identity, render_identity, write_identity_file, IdentitySnapshot,
};
use wintermute::executor::ExecutorKind;

fn sample_snapshot() -> IdentitySnapshot {
    IdentitySnapshot {
        model_id: "anthropic/claude-sonnet-4-5-20250929".to_owned(),
        executor_kind: ExecutorKind::Docker,
        has_network_isolation: true,
        core_tool_count: 8,
        dynamic_tool_count: 3,
        active_memory_count: 42,
        pending_memory_count: 5,
        has_vector_search: false,
        session_budget_limit: 500_000,
        daily_budget_limit: 5_000_000,
        uptime: Duration::from_secs(3_723),
    }
}

#[test]
fn render_identity_contains_all_sections() {
    let doc = render_identity(&sample_snapshot());
    assert!(doc.contains("# Wintermute"));
    assert!(doc.contains("## Your Architecture"));
    assert!(doc.contains("## Your Tools"));
    assert!(doc.contains("## Your Memory"));
    assert!(doc.contains("## Budget"));
    assert!(doc.contains("## Privacy Boundary"));
}

#[test]
fn render_identity_includes_model_id() {
    let doc = render_identity(&sample_snapshot());
    assert!(doc.contains("anthropic/claude-sonnet-4-5-20250929"));
}

#[test]
fn render_identity_shows_docker_executor() {
    let doc = render_identity(&sample_snapshot());
    assert!(doc.contains("Docker sandbox"));
}

#[test]
fn render_identity_shows_direct_executor() {
    let mut snap = sample_snapshot();
    snap.executor_kind = ExecutorKind::Direct;
    snap.has_network_isolation = false;
    let doc = render_identity(&snap);
    assert!(doc.contains("Direct mode"));
    assert!(doc.contains("without network isolation"));
}

#[test]
fn render_identity_shows_vector_search_status() {
    // Without vector search
    let doc = render_identity(&sample_snapshot());
    assert!(doc.contains("keyword search only"));

    // With vector search
    let mut snap = sample_snapshot();
    snap.has_vector_search = true;
    let doc = render_identity(&snap);
    assert!(doc.contains("vector search"));
    assert!(!doc.contains("not configured"));
}

#[test]
fn render_identity_shows_memory_counts() {
    let doc = render_identity(&sample_snapshot());
    assert!(doc.contains("42 active memories"));
    assert!(doc.contains("5 pending memories"));
}

#[test]
fn render_identity_shows_budget_limits() {
    let doc = render_identity(&sample_snapshot());
    assert!(doc.contains("500000"));
    assert!(doc.contains("5000000"));
}

#[test]
fn write_and_load_round_trip() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("IDENTITY.md");

    let content = render_identity(&sample_snapshot());
    write_identity_file(&content, &path).expect("write identity file");

    let loaded = load_identity(&path);
    assert_eq!(loaded, Some(content));
}

#[test]
fn load_identity_returns_none_for_missing_file() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("DOES_NOT_EXIST.md");
    assert!(load_identity(&path).is_none());
}

// ---------------------------------------------------------------------------
// format_uptime tests
// ---------------------------------------------------------------------------

#[test]
fn format_uptime_seconds() {
    assert_eq!(format_uptime(Duration::from_secs(45)), "0m 45s");
}

#[test]
fn format_uptime_minutes() {
    assert_eq!(format_uptime(Duration::from_secs(135)), "2m 15s");
}

#[test]
fn format_uptime_hours() {
    assert_eq!(format_uptime(Duration::from_secs(3_723)), "1h 2m");
}

#[test]
fn format_uptime_days() {
    assert_eq!(format_uptime(Duration::from_secs(90_000)), "1d 1h 0m");
}
