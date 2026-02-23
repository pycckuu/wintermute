//! System prompt assembly, conversation context trimming, and context compaction.
//!
//! The context module builds the system prompt from personality settings,
//! environment info, memory results, and current state. It also provides
//! token-aware conversation trimming to keep context within budget, and
//! LLM-based context compaction to extend long conversations.

use crate::executor::ExecutorKind;
use crate::memory::Memory;
use crate::providers::{Message, MessageContent, Role};

// ---------------------------------------------------------------------------
// Token estimation
// ---------------------------------------------------------------------------

/// Approximate bytes-per-token ratio for estimation.
///
/// English text averages roughly 4 bytes per token. For multi-byte UTF-8 this
/// overestimates token count, which is intentionally conservative.
const BYTES_PER_TOKEN: u64 = 4;

// ---------------------------------------------------------------------------
// System prompt assembly
// ---------------------------------------------------------------------------

/// Build a system prompt from all available context.
///
/// Sections included:
/// 1. Personality text (from `agent.toml`)
/// 2. System Identity Document (SID) — self-knowledge about architecture and state
/// 3. Environment: executor type and working directory context
/// 4. Tool availability summary
/// 5. Relevant memories (if any)
/// 6. Current context: date/time and pending approvals
pub fn assemble_system_prompt(
    personality: &str,
    identity_document: Option<&str>,
    executor_kind: ExecutorKind,
    dynamic_tool_count: usize,
    memories: &[Memory],
    pending_approvals: usize,
    current_time: &str,
) -> String {
    let mut sections: Vec<String> = Vec::new();

    // Section 1: Personality
    if !personality.is_empty() {
        sections.push(personality.to_owned());
    }

    // Section 2: System Identity Document
    if let Some(sid) = identity_document {
        if !sid.is_empty() {
            sections.push(sid.to_owned());
        }
    }

    // Section 3: Environment
    let env_label = match executor_kind {
        ExecutorKind::Docker => "Docker sandbox (network-isolated container)",
        ExecutorKind::Direct => "Direct (host-local, restricted)",
    };
    sections.push(format!("## Environment\nExecutor: {env_label}"));

    // Section 4: Tools
    sections.push(format!(
        "## Tools\nYou have access to core tools plus {dynamic_tool_count} dynamic tool(s)."
    ));

    // Section 5: Memories
    if !memories.is_empty() {
        let mut memory_section = String::from("## Relevant Memories\n");
        for mem in memories {
            let kind = mem.kind.as_str();
            memory_section.push_str(&format!("- [{kind}] {}\n", mem.content));
        }
        sections.push(memory_section);
    }

    // Section 6: Current context
    let mut ctx_section = format!("## Current Context\nDate/Time: {current_time}");
    if pending_approvals > 0 {
        ctx_section.push_str(&format!(
            "\nPending approvals: {pending_approvals} tool call(s) awaiting user confirmation"
        ));
    }
    sections.push(ctx_section);

    sections.join("\n\n")
}

// ---------------------------------------------------------------------------
// Conversation trimming
// ---------------------------------------------------------------------------

/// Trim a conversation to fit within a token budget.
///
/// Strategy:
/// - Always keep the first message (often sets conversation context)
/// - Always keep the last message (most recent user input)
/// - Drop oldest messages from the middle until under budget
/// - If only one message exists, always keep it
pub fn trim_messages(messages: &[Message], max_context_tokens: u64) -> Vec<Message> {
    if messages.is_empty() {
        return Vec::new();
    }

    let total_estimated = estimate_messages_tokens(messages);
    if total_estimated <= max_context_tokens {
        return messages.to_vec();
    }

    // Always keep first and last
    if messages.len() <= 2 {
        return messages.to_vec();
    }

    let first = &messages[0];
    let last = &messages[messages.len().saturating_sub(1)];
    let fixed_cost = estimate_message_tokens(first).saturating_add(estimate_message_tokens(last));

    if fixed_cost >= max_context_tokens {
        // Even first+last exceed budget; return just the last message
        return vec![last.clone()];
    }

    let mut remaining_budget = max_context_tokens.saturating_sub(fixed_cost);
    let middle = &messages[1..messages.len().saturating_sub(1)];

    // Walk backwards through the middle (keep most recent middle messages)
    let mut kept_middle: Vec<Message> = Vec::new();
    for msg in middle.iter().rev() {
        let cost = estimate_message_tokens(msg);
        if cost <= remaining_budget {
            kept_middle.push(msg.clone());
            remaining_budget = remaining_budget.saturating_sub(cost);
        } else {
            break;
        }
    }

    // Reverse to restore chronological order
    kept_middle.reverse();

    let mut result = Vec::with_capacity(kept_middle.len().saturating_add(2));
    result.push(first.clone());
    result.extend(kept_middle);
    result.push(last.clone());
    result
}

/// Trim messages aggressively to a fraction of the original budget.
///
/// Used when retrying after context overflow: keep first, last, and a reduced
/// middle section. `fraction` should be in (0.0, 1.0] (e.g. 0.5 = keep half).
pub fn trim_messages_to_fraction(
    messages: &[Message],
    max_context_tokens: u64,
    fraction: f64,
) -> Vec<Message> {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let reduced = (max_context_tokens as f64 * fraction) as u64;
    trim_messages(messages, reduced.max(100))
}

/// Estimate tokens for a slice of messages.
pub fn estimate_messages_tokens(messages: &[Message]) -> u64 {
    messages.iter().map(estimate_message_tokens).sum()
}

/// Estimate tokens for a single message using a byte-length heuristic.
fn estimate_message_tokens(message: &Message) -> u64 {
    let text = message.content.text();
    let byte_count = u64::try_from(text.len()).unwrap_or(u64::MAX);
    byte_count.saturating_add(BYTES_PER_TOKEN.saturating_sub(1)) / BYTES_PER_TOKEN
}

// ---------------------------------------------------------------------------
// Context compaction
// ---------------------------------------------------------------------------

/// Trigger compaction when session budget usage reaches this percentage.
pub const COMPACTION_TRIGGER_PERCENT: u8 = 60;

/// Minimum messages required before compaction is attempted.
const MIN_MESSAGES_FOR_COMPACTION: usize = 6;

/// Number of recent messages to always keep during compaction.
pub const COMPACTION_KEEP_LAST: usize = 4;

/// Plan describing how to split conversation for compaction.
#[derive(Debug, Clone)]
pub struct CompactionPlan {
    /// Messages to be summarized by the LLM.
    pub messages_to_compact: Vec<Message>,
    /// Messages to keep verbatim after compaction.
    pub messages_to_keep: Vec<Message>,
    /// Estimated token savings from compaction.
    pub estimated_savings_tokens: u64,
}

/// Check whether context compaction should be triggered.
pub fn should_compact(session_percent: u8) -> bool {
    session_percent >= COMPACTION_TRIGGER_PERCENT
}

/// Build a compaction plan from the conversation history.
///
/// Keeps the first message and the last `keep_last` messages verbatim.
/// Everything in between is marked for summarization.
///
/// Returns `None` if the conversation is too short for compaction.
pub fn build_compaction_plan(messages: &[Message], keep_last: usize) -> Option<CompactionPlan> {
    if messages.len() < MIN_MESSAGES_FOR_COMPACTION {
        return None;
    }

    let keep_last = keep_last.min(messages.len().saturating_sub(2));

    // First message + compactable middle + last N
    let split_point = messages.len().saturating_sub(keep_last);
    if split_point <= 1 {
        return None;
    }

    let to_compact = messages[1..split_point].to_vec();
    if to_compact.is_empty() {
        return None;
    }

    let mut to_keep = vec![messages[0].clone()];
    to_keep.extend_from_slice(&messages[split_point..]);

    let compact_tokens = estimate_messages_tokens(&to_compact);

    Some(CompactionPlan {
        messages_to_compact: to_compact,
        messages_to_keep: to_keep,
        estimated_savings_tokens: compact_tokens,
    })
}

/// Build the LLM request messages for compacting a conversation.
///
/// Returns a minimal prompt asking the LLM to summarize the conversation chunk.
pub fn build_compaction_request(plan: &CompactionPlan, target_tokens: u64) -> Vec<Message> {
    let mut conversation_text = String::new();
    for msg in &plan.messages_to_compact {
        let role_label = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => "System",
            Role::Tool => "Tool",
        };
        let text = msg.content.text();
        conversation_text.push_str(&format!("{role_label}: {text}\n\n"));
    }

    vec![Message {
        role: Role::User,
        content: MessageContent::Text(format!(
            "Summarize this conversation so far, preserving:\n\
                 - All decisions made\n\
                 - All action items and their status\n\
                 - Current task state\n\
                 - Key facts, file paths, and code context mentioned\n\
                 Keep it under {target_tokens} tokens. Be concise but complete.\n\n\
                 ---\n\n\
                 {conversation_text}"
        )),
    }]
}

/// Apply compaction by replacing old messages with a summary.
///
/// The summary becomes the second message (after the first system/context message),
/// followed by the kept recent messages.
pub fn apply_compaction(summary: &str, kept_messages: Vec<Message>) -> Vec<Message> {
    if kept_messages.is_empty() {
        return vec![Message {
            role: Role::Assistant,
            content: MessageContent::Text(format!("[Conversation summary]\n{summary}")),
        }];
    }

    let mut result = Vec::with_capacity(kept_messages.len().saturating_add(1));
    // Keep the first message (typically system context)
    result.push(kept_messages[0].clone());
    // Insert summary as an assistant message
    result.push(Message {
        role: Role::Assistant,
        content: MessageContent::Text(format!("[Conversation summary]\n{summary}")),
    });
    // Append remaining kept messages
    if kept_messages.len() > 1 {
        result.extend_from_slice(&kept_messages[1..]);
    }
    result
}
