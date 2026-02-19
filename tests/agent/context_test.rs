//! Tests for context assembly and conversation trimming.

use wintermute::agent::context::{assemble_system_prompt, trim_messages};
use wintermute::executor::ExecutorKind;
use wintermute::memory::{Memory, MemoryKind, MemorySource, MemoryStatus};
use wintermute::providers::{Message, MessageContent, Role};

// ---------------------------------------------------------------------------
// assemble_system_prompt tests
// ---------------------------------------------------------------------------

#[test]
fn system_prompt_includes_personality() {
    let prompt = assemble_system_prompt(
        "You are a helpful AI.",
        ExecutorKind::Docker,
        0,
        &[],
        0,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(prompt.contains("You are a helpful AI."));
}

#[test]
fn system_prompt_includes_docker_executor() {
    let prompt = assemble_system_prompt(
        "",
        ExecutorKind::Docker,
        0,
        &[],
        0,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(prompt.contains("Docker sandbox"));
}

#[test]
fn system_prompt_includes_direct_executor() {
    let prompt = assemble_system_prompt(
        "",
        ExecutorKind::Direct,
        0,
        &[],
        0,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(prompt.contains("Direct"));
}

#[test]
fn system_prompt_includes_memories_when_provided() {
    let memories = vec![
        Memory {
            id: Some(1),
            kind: MemoryKind::Fact,
            content: "User prefers dark mode".to_owned(),
            metadata: None,
            status: MemoryStatus::Active,
            source: MemorySource::User,
            created_at: None,
            updated_at: None,
        },
        Memory {
            id: Some(2),
            kind: MemoryKind::Procedure,
            content: "Deploy with cargo build --release".to_owned(),
            metadata: None,
            status: MemoryStatus::Active,
            source: MemorySource::Agent,
            created_at: None,
            updated_at: None,
        },
    ];

    let prompt = assemble_system_prompt(
        "personality",
        ExecutorKind::Docker,
        0,
        &memories,
        0,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(prompt.contains("Relevant Memories"));
    assert!(prompt.contains("User prefers dark mode"));
    assert!(prompt.contains("[fact]"));
    assert!(prompt.contains("[procedure]"));
    assert!(prompt.contains("Deploy with cargo build --release"));
}

#[test]
fn system_prompt_omits_memory_section_when_empty() {
    let prompt = assemble_system_prompt(
        "personality",
        ExecutorKind::Docker,
        0,
        &[],
        0,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(!prompt.contains("Relevant Memories"));
}

#[test]
fn system_prompt_includes_pending_approvals() {
    let prompt = assemble_system_prompt(
        "personality",
        ExecutorKind::Docker,
        0,
        &[],
        3,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(prompt.contains("Pending approvals: 3"));
}

#[test]
fn system_prompt_omits_approvals_when_zero() {
    let prompt = assemble_system_prompt(
        "personality",
        ExecutorKind::Docker,
        0,
        &[],
        0,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(!prompt.contains("Pending approvals"));
}

#[test]
fn system_prompt_includes_current_time() {
    let prompt = assemble_system_prompt(
        "",
        ExecutorKind::Docker,
        0,
        &[],
        0,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(prompt.contains("2026-02-19 12:00:00 UTC"));
}

#[test]
fn system_prompt_includes_dynamic_tool_count() {
    let prompt = assemble_system_prompt(
        "",
        ExecutorKind::Docker,
        5,
        &[],
        0,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(prompt.contains("5 dynamic tool(s)"));
}

// ---------------------------------------------------------------------------
// trim_messages tests
// ---------------------------------------------------------------------------

fn make_message(role: Role, text: &str) -> Message {
    Message {
        role,
        content: MessageContent::Text(text.to_owned()),
    }
}

#[test]
fn trim_messages_preserves_all_when_under_budget() {
    let messages = vec![
        make_message(Role::User, "Hello"),
        make_message(Role::Assistant, "Hi there"),
        make_message(Role::User, "How are you?"),
    ];

    // Large budget — all should fit
    let trimmed = trim_messages(&messages, 1_000_000);
    assert_eq!(trimmed.len(), 3);
    assert_eq!(trimmed[0].content.text(), "Hello");
    assert_eq!(trimmed[2].content.text(), "How are you?");
}

#[test]
fn trim_messages_keeps_first_and_last_when_over_budget() {
    // Create messages that exceed a small budget
    let messages = vec![
        make_message(Role::User, "First message"),
        make_message(Role::Assistant, "A very long middle message that takes many tokens to represent in the context window and should be dropped"),
        make_message(Role::Assistant, "Another middle message that is also quite long and should be considered for trimming"),
        make_message(Role::User, "Last message"),
    ];

    // Small budget that can only hold first + last
    let trimmed = trim_messages(&messages, 10);
    assert!(trimmed.len() >= 2);
    assert_eq!(trimmed[0].content.text(), "First message");
    assert_eq!(
        trimmed[trimmed.len().saturating_sub(1)].content.text(),
        "Last message"
    );
}

#[test]
fn trim_messages_empty_returns_empty() {
    let trimmed = trim_messages(&[], 1000);
    assert!(trimmed.is_empty());
}

#[test]
fn trim_messages_single_message_always_kept() {
    let messages = vec![make_message(Role::User, "Only message")];
    let trimmed = trim_messages(&messages, 1);
    assert_eq!(trimmed.len(), 1);
    assert_eq!(trimmed[0].content.text(), "Only message");
}

#[test]
fn trim_messages_two_messages_always_kept() {
    let messages = vec![
        make_message(Role::User, "First"),
        make_message(Role::Assistant, "Second"),
    ];
    let trimmed = trim_messages(&messages, 1);
    assert_eq!(trimmed.len(), 2);
}

#[test]
fn trim_messages_prefers_recent_messages() {
    let messages = vec![
        make_message(Role::User, "msg1"),
        make_message(Role::Assistant, "old"),
        make_message(Role::Assistant, "mid"),
        make_message(Role::Assistant, "recent"),
        make_message(Role::User, "last"),
    ];

    // Budget enough for first + last + one middle message (~4 tokens per short msg)
    // Each short message is ~1 token. Total needed for all 5: ~5 tokens.
    // Budget of 4 should drop the oldest middle messages.
    let trimmed = trim_messages(&messages, 4);

    // First and last must always be present
    assert_eq!(trimmed[0].content.text(), "msg1");
    assert_eq!(
        trimmed[trimmed.len().saturating_sub(1)].content.text(),
        "last"
    );

    // If "recent" is present but "old" is not, trimming correctly favours recent
    let has_recent = trimmed.iter().any(|m| m.content.text() == "recent");
    let has_old = trimmed.iter().any(|m| m.content.text() == "old");
    if trimmed.len() < 5 {
        // With budget pressure, recent should be preferred over old
        assert!(has_recent || !has_old, "should prefer recent over old");
    }
}
