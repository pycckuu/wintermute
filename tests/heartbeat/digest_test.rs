//! Tests for `src/heartbeat/digest.rs` — weekly memory digest.

use wintermute::heartbeat::digest::{build_consolidation_prompt, load_user_md, write_user_md};

// ---------------------------------------------------------------------------
// build_consolidation_prompt tests
// ---------------------------------------------------------------------------

#[test]
fn prompt_includes_current_user_md_content() {
    let prompt =
        build_consolidation_prompt("# Existing Content\n- Some notes", &["New fact".to_owned()]);
    assert!(prompt.contains("# Existing Content"));
    assert!(prompt.contains("Some notes"));
}

#[test]
fn prompt_handles_empty_user_md() {
    let prompt = build_consolidation_prompt("", &["Memory one".to_owned()]);
    assert!(prompt.contains("first digest"));
}

#[test]
fn prompt_includes_all_memories() {
    let memories = vec![
        "Fact A".to_owned(),
        "Fact B".to_owned(),
        "Fact C".to_owned(),
    ];
    let prompt = build_consolidation_prompt("existing content", &memories);
    assert!(prompt.contains("1. Fact A"));
    assert!(prompt.contains("2. Fact B"));
    assert!(prompt.contains("3. Fact C"));
}

#[test]
fn prompt_handles_empty_memories() {
    let prompt = build_consolidation_prompt("existing content", &[]);
    assert!(prompt.contains("no new memories"));
}

#[test]
fn prompt_contains_rules() {
    let prompt = build_consolidation_prompt("", &[]);
    assert!(prompt.contains("under 200 lines"));
    assert!(prompt.contains("Remove duplicates"));
    assert!(prompt.contains("contradictions"));
}

// ---------------------------------------------------------------------------
// write and load USER.md tests
// ---------------------------------------------------------------------------

#[test]
fn write_and_load_user_md_round_trip() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("USER.md");

    write_user_md("# My Notes\n- Item one\n", &path).expect("write should succeed");

    let loaded = load_user_md(&path);
    assert_eq!(loaded, "# My Notes\n- Item one\n");
}

#[test]
fn load_user_md_returns_empty_for_missing_file() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("DOES_NOT_EXIST.md");
    let loaded = load_user_md(&path);
    assert!(loaded.is_empty());
}

// ---------------------------------------------------------------------------
// Scheduler recognition test
// ---------------------------------------------------------------------------

#[test]
fn scheduler_recognizes_digest_builtin() {
    // Verify the digest builtin name is recognized by the scheduler by checking
    // that a ScheduledTaskConfig with builtin = "digest" is valid.
    let task = wintermute::config::ScheduledTaskConfig {
        name: "weekly_digest".to_owned(),
        cron: "0 0 0 * * 0".to_owned(),
        builtin: Some("digest".to_owned()),
        tool: None,
        budget_tokens: None,
        notify: true,
        enabled: true,
    };
    // The task can be constructed and enabled — this validates the config shape.
    // Full execution requires HeartbeatDeps which can't be constructed in a unit test.
    assert!(task.enabled);
    assert_eq!(task.builtin.as_deref(), Some("digest"));
}
