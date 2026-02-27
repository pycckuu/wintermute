//! Tests for context assembly, conversation trimming, and compaction.

use std::path::PathBuf;

use wintermute::agent::context::{
    apply_compaction, assemble_system_prompt, build_compaction_plan, build_compaction_request,
    should_compact, trim_messages, trim_messages_to_fraction,
};
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
        None,
        None,
        None,
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
        None,
        None,
        None,
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
        None,
        None,
        None,
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
        None,
        None,
        None,
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
        None,
        None,
        None,
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
        None,
        None,
        None,
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
        None,
        None,
        None,
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
        None,
        None,
        None,
        ExecutorKind::Docker,
        0,
        &[],
        0,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(prompt.contains("2026-02-19 12:00:00 UTC"));
}

#[test]
fn system_prompt_includes_identity_document_when_provided() {
    let prompt = assemble_system_prompt(
        "personality",
        Some("# Wintermute\nYou are Wintermute."),
        None,
        None,
        ExecutorKind::Docker,
        0,
        &[],
        0,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(prompt.contains("# Wintermute"));
    assert!(prompt.contains("You are Wintermute."));
}

#[test]
fn system_prompt_includes_dynamic_tool_count() {
    let prompt = assemble_system_prompt(
        "",
        None,
        None,
        None,
        ExecutorKind::Docker,
        5,
        &[],
        0,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(prompt.contains("5 dynamic tool(s)"));
}

#[test]
fn system_prompt_includes_user_md_when_provided() {
    let prompt = assemble_system_prompt(
        "personality",
        None,
        None,
        Some("# Preferences\n- Dark mode\n- Vim keybindings"),
        ExecutorKind::Docker,
        0,
        &[],
        0,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(prompt.contains("Long-Term Memory"));
    assert!(prompt.contains("Dark mode"));
    assert!(prompt.contains("Vim keybindings"));
}

#[test]
fn system_prompt_omits_user_md_when_empty() {
    let prompt = assemble_system_prompt(
        "personality",
        None,
        None,
        Some(""),
        ExecutorKind::Docker,
        0,
        &[],
        0,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(!prompt.contains("Long-Term Memory"));
}

#[test]
fn system_prompt_includes_agents_md_when_provided() {
    let prompt = assemble_system_prompt(
        "personality",
        None,
        Some("- Always validate tool input before execution\n- Use JSON for structured output"),
        None,
        ExecutorKind::Docker,
        0,
        &[],
        0,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(prompt.contains("Lessons Learned"));
    assert!(prompt.contains("Always validate tool input"));
}

#[test]
fn system_prompt_omits_agents_md_when_empty() {
    let prompt = assemble_system_prompt(
        "personality",
        None,
        Some(""),
        None,
        ExecutorKind::Docker,
        0,
        &[],
        0,
        "2026-02-19 12:00:00 UTC",
    );
    assert!(!prompt.contains("Lessons Learned"));
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

// ---------------------------------------------------------------------------
// trim_messages_to_fraction tests (overflow retry)
// ---------------------------------------------------------------------------

#[test]
fn trim_messages_to_fraction_reduces_budget() {
    let messages = vec![
        make_message(Role::User, "First"),
        make_message(Role::Assistant, "Middle content"),
        make_message(Role::User, "Last"),
    ];

    let trimmed = trim_messages_to_fraction(&messages, 1000, 0.5);
    assert!(trimmed.len() <= 3);
    assert_eq!(trimmed[0].content.text(), "First");
    assert_eq!(
        trimmed[trimmed.len().saturating_sub(1)].content.text(),
        "Last"
    );
}

#[test]
fn trim_messages_to_fraction_enforces_minimum_budget() {
    let messages = vec![
        make_message(Role::User, "First"),
        make_message(Role::User, "Last"),
    ];
    let trimmed = trim_messages_to_fraction(&messages, 50, 0.1);
    assert!(!trimmed.is_empty());
}

// ---------------------------------------------------------------------------
// should_compact tests
// ---------------------------------------------------------------------------

#[test]
fn should_compact_below_threshold() {
    assert!(!should_compact(0));
    assert!(!should_compact(30));
    assert!(!should_compact(59));
}

#[test]
fn should_compact_at_threshold() {
    assert!(should_compact(60));
}

#[test]
fn should_compact_above_threshold() {
    assert!(should_compact(75));
    assert!(should_compact(100));
}

// ---------------------------------------------------------------------------
// build_compaction_plan tests
// ---------------------------------------------------------------------------

#[test]
fn compaction_plan_returns_none_for_short_conversations() {
    let messages = vec![
        make_message(Role::User, "Hi"),
        make_message(Role::Assistant, "Hello"),
        make_message(Role::User, "Bye"),
    ];
    // Too few messages (< 6)
    assert!(build_compaction_plan(&messages, 4).is_none());
}

#[test]
fn compaction_plan_splits_correctly() {
    let messages = vec![
        make_message(Role::User, "First"),       // 0: kept (first)
        make_message(Role::Assistant, "Second"), // 1: compacted
        make_message(Role::User, "Third"),       // 2: compacted
        make_message(Role::Assistant, "Fourth"), // 3: compacted
        make_message(Role::User, "Fifth"),       // 4: kept (last 2)
        make_message(Role::Assistant, "Sixth"),  // 5: kept (last 2)
    ];

    let plan = build_compaction_plan(&messages, 2).expect("should produce a plan");

    // First message is always kept
    assert_eq!(plan.messages_to_keep[0].content.text(), "First");
    // Last 2 messages kept
    assert!(plan
        .messages_to_keep
        .iter()
        .any(|m| m.content.text() == "Fifth"));
    assert!(plan
        .messages_to_keep
        .iter()
        .any(|m| m.content.text() == "Sixth"));

    // Middle messages compacted
    assert_eq!(plan.messages_to_compact.len(), 3);
    assert_eq!(plan.messages_to_compact[0].content.text(), "Second");
    assert_eq!(plan.messages_to_compact[1].content.text(), "Third");
    assert_eq!(plan.messages_to_compact[2].content.text(), "Fourth");

    assert!(plan.estimated_savings_tokens > 0);
}

#[test]
fn compaction_plan_keeps_first_message_separate() {
    let messages: Vec<Message> = (0..8)
        .map(|i| {
            let role = if i % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            };
            make_message(role, &format!("msg{i}"))
        })
        .collect();

    let plan = build_compaction_plan(&messages, 4).expect("should produce a plan");

    // First message always in kept set
    assert_eq!(plan.messages_to_keep[0].content.text(), "msg0");
    // First message NOT in compact set
    assert!(!plan
        .messages_to_compact
        .iter()
        .any(|m| m.content.text() == "msg0"));
}

// ---------------------------------------------------------------------------
// build_compaction_request tests
// ---------------------------------------------------------------------------

#[test]
fn compaction_request_includes_conversation_text() {
    let messages = vec![
        make_message(Role::User, "First"),
        make_message(Role::Assistant, "Reply one"),
        make_message(Role::User, "Question"),
        make_message(Role::Assistant, "Reply two"),
        make_message(Role::User, "Follow up"),
        make_message(Role::Assistant, "Last reply"),
    ];

    let plan = build_compaction_plan(&messages, 2).expect("plan");
    let request = build_compaction_request(&plan, 500);

    assert_eq!(request.len(), 1);
    assert_eq!(request[0].role, Role::User);
    let text = request[0].content.text();
    assert!(text.contains("Summarize"));
    assert!(text.contains("500"));
    assert!(text.contains("Reply one"));
}

// ---------------------------------------------------------------------------
// apply_compaction tests
// ---------------------------------------------------------------------------

#[test]
fn apply_compaction_inserts_summary_after_first() {
    let kept = vec![
        make_message(Role::User, "First"),
        make_message(Role::User, "Recent1"),
        make_message(Role::Assistant, "Recent2"),
    ];

    let result = apply_compaction("Summary of conversation", kept);

    assert_eq!(result.len(), 4);
    assert_eq!(result[0].content.text(), "First");
    assert!(result[1].content.text().contains("[Conversation summary]"));
    assert!(result[1].content.text().contains("Summary of conversation"));
    assert_eq!(result[1].role, Role::Assistant);
    assert_eq!(result[2].content.text(), "Recent1");
    assert_eq!(result[3].content.text(), "Recent2");
}

#[test]
fn apply_compaction_handles_empty_kept() {
    let result = apply_compaction("Orphan summary", Vec::new());

    assert_eq!(result.len(), 1);
    assert!(result[0].content.text().contains("Orphan summary"));
    assert_eq!(result[0].role, Role::Assistant);
}

#[test]
fn apply_compaction_handles_single_kept() {
    let kept = vec![make_message(Role::User, "Only message")];
    let result = apply_compaction("Summary text", kept);

    assert_eq!(result.len(), 2);
    assert_eq!(result[0].content.text(), "Only message");
    assert!(result[1].content.text().contains("Summary text"));
}

#[test]
fn compaction_summary_is_redacted_before_apply_compaction() -> Result<(), Box<dyn std::error::Error>>
{
    let loop_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/agent/loop.rs");
    let content = std::fs::read_to_string(loop_src)?;

    let redact_idx = content
        .find("redactor().redact(&summary)")
        .ok_or("missing compaction summary redaction call")?;
    let apply_idx = content
        .find("apply_compaction(&redacted_summary")
        .ok_or("missing apply_compaction call with redacted summary")?;

    assert!(
        redact_idx < apply_idx,
        "compaction summary redaction must occur before apply_compaction"
    );
    Ok(())
}
