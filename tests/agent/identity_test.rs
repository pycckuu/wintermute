//! Tests for the System Identity Document (SID) generator.

use std::time::Duration;

use wintermute::agent::identity::{
    format_uptime, load_identity, render_identity, write_identity_file, IdentitySnapshot,
};
use wintermute::executor::ExecutorKind;
use wintermute::tools::browser::BrowserMode;

fn sample_snapshot() -> IdentitySnapshot {
    IdentitySnapshot {
        version: "0.6.0".to_owned(),
        model_id: "anthropic/claude-sonnet-4-5-20250929".to_owned(),
        executor_kind: ExecutorKind::Docker,
        core_tool_count: 9,
        dynamic_tool_count: 3,
        active_memory_count: 42,
        pending_memory_count: 5,
        has_vector_search: false,
        session_budget_limit: 500_000,
        daily_budget_limit: 5_000_000,
        uptime: Duration::from_secs(3_723),
        agent_name: "Wintermute".to_owned(),
        browser_mode: BrowserMode::None,
        oracle_model: None,
        soul_modification_mode: wintermute::config::SoulModificationMode::default(),
        docs_count: 0,
        scheduled_task_summaries: Vec::new(),
        dynamic_tool_summaries: Vec::new(),
    }
}

#[test]
fn render_identity_contains_all_sections() {
    let doc = render_identity(&sample_snapshot());
    assert!(doc.contains("# Wintermute"));
    assert!(doc.contains("## Your Architecture"));
    assert!(doc.contains("## Topology"));
    assert!(doc.contains("## Your Tools"));
    assert!(doc.contains("## Browser"));
    assert!(doc.contains("## Your Memory"));
    assert!(doc.contains("## Budget"));
    assert!(doc.contains("## Privacy Boundary"));
    assert!(doc.contains("## What You Can Modify About Yourself"));
    assert!(doc.contains("## What You CANNOT Modify"));
    assert!(doc.contains("## Self-Modification Protocol"));
    assert!(doc.contains("## What You Can Help Set Up"));
    assert!(doc.contains("## Handling Non-Text Messages"));
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

#[test]
fn render_identity_lists_docker_manage_and_save_to() {
    let doc = render_identity(&sample_snapshot());
    assert!(
        doc.contains("docker_manage"),
        "should list docker_manage in tools"
    );
    assert!(doc.contains("save_to"), "should mention web_fetch save_to");
}

#[test]
fn render_identity_shows_topology_for_docker() {
    let doc = render_identity(&sample_snapshot());
    assert!(doc.contains("egress-proxy"));
    assert!(doc.contains("sandbox"));
    assert!(doc.contains("service containers"));
    assert!(doc.contains("browser"));
}

#[test]
fn render_identity_omits_topology_for_direct() {
    let mut snap = sample_snapshot();
    snap.executor_kind = ExecutorKind::Direct;
    let doc = render_identity(&snap);
    assert!(
        !doc.contains("## Topology"),
        "direct mode should not show topology"
    );
}

#[test]
fn render_identity_contains_media_handling_guidance() {
    let doc = render_identity(&sample_snapshot());
    assert!(doc.contains("## Handling Non-Text Messages"));
    assert!(doc.contains("create_tool"));
    assert!(doc.contains("whisper"));
    assert!(doc.contains("multimodal model"));
    assert!(doc.contains("pypdf"));
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

#[test]
fn render_identity_uses_custom_agent_name() {
    let mut snap = sample_snapshot();
    snap.agent_name = "Neuromancer".to_owned();
    let doc = render_identity(&snap);
    assert!(doc.contains("# Neuromancer"));
    assert!(doc.contains("You are Neuromancer, a self-coding AI agent."));
    assert!(!doc.contains("Wintermute"));
}

#[test]
fn render_identity_contains_self_modification_sections() {
    let doc = render_identity(&sample_snapshot());
    assert!(doc.contains("[personality]"));
    assert!(doc.contains("rename yourself"));
    assert!(doc.contains("config.toml"));
    assert!(doc.contains("IDENTITY.md"));
    assert!(doc.contains("evolve:"));
}

#[test]
fn render_identity_shows_browser_none() {
    let doc = render_identity(&sample_snapshot());
    assert!(doc.contains("## Browser"));
    assert!(doc.contains("No browser available."));
}

#[test]
fn render_identity_shows_browser_attached() {
    let mut snap = sample_snapshot();
    snap.browser_mode = BrowserMode::Attached { port: 9222 };
    let doc = render_identity(&snap);
    assert!(doc.contains("## Browser"));
    assert!(doc.contains("Connected to your Chrome on port 9222"));
    assert!(doc.contains("won't submit"));
    assert!(doc.contains("won't type passwords"));
}

#[test]
fn render_identity_shows_browser_standalone() {
    let mut snap = sample_snapshot();
    snap.browser_mode = BrowserMode::Standalone { port: 9223 };
    let doc = render_identity(&snap);
    assert!(doc.contains("## Browser"));
    assert!(doc.contains("standalone browser"));
    assert!(doc.contains("--remote-debugging-port=9222"));
}

#[test]
fn render_identity_includes_escalation_with_oracle() {
    let mut snap = sample_snapshot();
    snap.oracle_model = Some("anthropic/claude-opus-4-20250514".to_owned());
    let doc = render_identity(&snap);
    assert!(doc.contains("## Escalation"));
    assert!(doc.contains("escalate"));
    assert!(doc.contains("claude-opus-4-20250514"));
}

#[test]
fn render_identity_escalation_without_oracle() {
    let doc = render_identity(&sample_snapshot());
    assert!(doc.contains("## Escalation"));
    assert!(doc.contains("No oracle model configured"));
}

#[test]
fn render_identity_includes_soul_modification_mode() {
    // Default mode is Notify.
    let doc = render_identity(&sample_snapshot());
    assert!(doc.contains("## Self-Modification Protocol"));
    assert!(doc.contains("notify"));

    // Approve mode.
    let mut snap = sample_snapshot();
    snap.soul_modification_mode = wintermute::config::SoulModificationMode::Approve;
    let doc = render_identity(&snap);
    assert!(doc.contains("approve"));
    assert!(doc.contains("Wait for explicit approval"));
}

#[test]
fn render_identity_includes_tool_stats() {
    let mut snap = sample_snapshot();
    snap.dynamic_tool_summaries = vec![
        (
            "weather".to_owned(),
            "Get weather data".to_owned(),
            15,
            0.93,
        ),
        (
            "calculator".to_owned(),
            "Math operations".to_owned(),
            3,
            1.0,
        ),
    ];
    let doc = render_identity(&snap);
    assert!(doc.contains("Custom Tool Stats"));
    assert!(doc.contains("`weather`"));
    assert!(doc.contains("invocations: 15"));
}

#[test]
fn render_identity_includes_silence_section() {
    let doc = render_identity(&sample_snapshot());
    assert!(doc.contains("## Silence"));
    assert!(doc.contains("[NO_REPLY]"));
}

#[test]
fn render_identity_includes_docs_section_when_present() {
    let mut snap = sample_snapshot();
    snap.docs_count = 3;
    let doc = render_identity(&snap);
    assert!(doc.contains("## Documentation"));
    assert!(doc.contains("3 doc(s)"));
}

#[test]
fn render_identity_omits_docs_section_when_empty() {
    let doc = render_identity(&sample_snapshot());
    assert!(!doc.contains("## Documentation"));
}

#[test]
fn render_identity_includes_scheduled_tasks() {
    let mut snap = sample_snapshot();
    snap.scheduled_task_summaries =
        vec!["daily_backup (builtin: backup, cron: 0 0 3 * * *)".to_owned()];
    let doc = render_identity(&snap);
    assert!(doc.contains("## Scheduled Tasks"));
    assert!(doc.contains("daily_backup"));
}

#[test]
fn render_identity_includes_escalate_in_tools_list() {
    let doc = render_identity(&sample_snapshot());
    assert!(
        doc.contains("escalate"),
        "core tools list should include escalate"
    );
}
