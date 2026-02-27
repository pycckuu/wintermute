//! Post-session reflection on tools created or modified during a session.
//!
//! When enabled (`learning.reflection = true`), the observer runs a lightweight
//! LLM call after staging to reflect on tools the agent created or modified.
//! The reflection is saved as a procedure memory for future reference.

use anyhow::Context;
use tracing::{debug, info};

use crate::agent::budget::DailyBudget;
use crate::executor::redactor::Redactor;
use crate::memory::{Memory, MemoryEngine, MemoryKind, MemorySource, MemoryStatus};
use crate::providers::router::ModelRouter;
use crate::providers::{extract_text, CompletionRequest, Message, MessageContent, Role};

/// Estimated tokens per reflection call (for budget pre-check).
const ESTIMATED_REFLECTION_TOKENS: u64 = 2000;

/// System prompt for the reflection model.
const REFLECTION_SYSTEM_PROMPT: &str = "\
You are reviewing tools that were created or modified during an agent session.
For each tool, provide a brief, actionable reflection: what could be improved,
edge cases that might be missed, or simpler approaches. Keep your response to
one paragraph per tool. Be specific and constructive.";

/// Reflect on tools created/modified during a session.
///
/// Calls the observer model with a lightweight prompt asking for improvement
/// suggestions. The reflection is saved as a procedure memory tagged with
/// the tool names.
///
/// # Errors
///
/// Returns an error if the LLM call or memory save fails.
pub async fn reflect_on_tools(
    tool_names: &[String],
    router: &ModelRouter,
    daily_budget: &DailyBudget,
    memory: &MemoryEngine,
    redactor: &Redactor,
) -> anyhow::Result<()> {
    if tool_names.is_empty() {
        return Ok(());
    }

    // Resolve the observer model (cheap/local).
    let provider = router
        .resolve(Some("observer"), None)
        .context("failed to resolve observer model for reflection")?;

    // Budget pre-check.
    daily_budget
        .check(ESTIMATED_REFLECTION_TOKENS)
        .context("reflection budget exceeded")?;

    let tools_list = tool_names.join(", ");
    debug!(tools = %tools_list, "starting post-session reflection");

    let user_prompt = format!(
        "Review the following tool(s) created/modified in this session: {tools_list}. \
         What could be improved? Edge cases missed? Simpler approach?"
    );

    let request = CompletionRequest {
        messages: vec![Message {
            role: Role::User,
            content: MessageContent::Text(user_prompt),
        }],
        system: Some(REFLECTION_SYSTEM_PROMPT.to_owned()),
        tools: vec![],
        max_tokens: Some(1024),
        stop_sequences: vec![],
    };

    let response = provider
        .complete(request)
        .await
        .context("reflection LLM call failed")?;

    // Record actual token usage.
    let total = u64::from(response.usage.input_tokens)
        .saturating_add(u64::from(response.usage.output_tokens));
    daily_budget.record(total);

    // Extract and redact response text.
    let response_text = extract_text(&response.content);

    if response_text.is_empty() {
        debug!("reflection produced empty response");
        return Ok(());
    }

    let redacted = redactor.redact(&response_text);

    // Save as procedure memory.
    let mem = Memory {
        id: None,
        kind: MemoryKind::Procedure,
        content: redacted,
        metadata: Some(serde_json::json!({
            "source": "reflection",
            "tools": tool_names,
        })),
        status: MemoryStatus::Pending,
        source: MemorySource::Observer,
        created_at: None,
        updated_at: None,
    };

    memory
        .save_memory(mem)
        .await
        .context("failed to save reflection memory")?;

    info!(tools = %tools_list, "post-session reflection saved");
    Ok(())
}
