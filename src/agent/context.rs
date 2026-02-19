//! System prompt assembly and conversation context trimming.
//!
//! The context module builds the system prompt from personality settings,
//! environment info, memory results, and current state. It also provides
//! token-aware conversation trimming to keep context within budget.

use crate::executor::ExecutorKind;
use crate::memory::Memory;
use crate::providers::Message;

// ---------------------------------------------------------------------------
// Token estimation
// ---------------------------------------------------------------------------

/// Approximate tokens-per-character ratio for estimation.
///
/// English text averages roughly 4 characters per token. This is intentionally
/// conservative (overestimates token count) to avoid exceeding limits.
const CHARS_PER_TOKEN: u64 = 4;

// ---------------------------------------------------------------------------
// System prompt assembly
// ---------------------------------------------------------------------------

/// Build a system prompt from all available context.
///
/// Sections included:
/// 1. Personality text (from `agent.toml`)
/// 2. Environment: executor type and working directory context
/// 3. Tool availability summary
/// 4. Relevant memories (if any)
/// 5. Current context: date/time and pending approvals
pub fn assemble_system_prompt(
    personality: &str,
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

    // Section 2: Environment
    let env_label = match executor_kind {
        ExecutorKind::Docker => "Docker sandbox (network-isolated container)",
        ExecutorKind::Direct => "Direct (host-local, restricted)",
    };
    sections.push(format!("## Environment\nExecutor: {env_label}"));

    // Section 3: Tools
    sections.push(format!(
        "## Tools\nYou have access to core tools plus {dynamic_tool_count} dynamic tool(s)."
    ));

    // Section 4: Memories
    if !memories.is_empty() {
        let mut memory_section = String::from("## Relevant Memories\n");
        for mem in memories {
            let kind = mem.kind.as_str();
            memory_section.push_str(&format!("- [{kind}] {}\n", mem.content));
        }
        sections.push(memory_section);
    }

    // Section 5: Current context
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

/// Estimate tokens for a slice of messages.
pub fn estimate_messages_tokens(messages: &[Message]) -> u64 {
    messages.iter().map(estimate_message_tokens).sum()
}

/// Estimate tokens for a single message using the 4-chars-per-token heuristic.
fn estimate_message_tokens(message: &Message) -> u64 {
    let text = message.content.text();
    let char_count = u64::try_from(text.len()).unwrap_or(u64::MAX);
    char_count.saturating_add(CHARS_PER_TOKEN.saturating_sub(1)) / CHARS_PER_TOKEN
}
