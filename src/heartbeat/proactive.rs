//! Proactive behavior checks between user interactions.
//!
//! When enabled (`heartbeat.proactive = true`), the heartbeat periodically
//! runs a lightweight LLM call to determine if the agent should take any
//! proactive action (health-check a flaky tool, prepare for a scheduled task,
//! or check in with the user).

use anyhow::Context;
use tracing::{debug, info};

use crate::agent::budget::DailyBudget;
use crate::providers::router::ModelRouter;
use crate::providers::{extract_text, CompletionRequest, Message, MessageContent, Role};

/// System prompt for proactive behavior checks.
const PROACTIVE_SYSTEM_PROMPT: &str = "\
You are a proactive agent assistant. Given the current context, decide if you \
should take any action right now. Consider: checking in with the user, \
health-checking a flaky tool, preparing for an upcoming scheduled task, or \
doing nothing.

If there is nothing useful to do, respond with exactly: [NO_REPLY]
If there is something useful, describe the action briefly (1-2 sentences).";

/// Run a lightweight proactive check using the observer model.
///
/// Builds a small context (~1K tokens) and asks the model whether any
/// proactive action should be taken. Returns `None` if the model says
/// `[NO_REPLY]`, or `Some(action)` with the suggested action text.
///
/// # Errors
///
/// Returns an error if the provider is unavailable, budget is exceeded,
/// or the LLM call fails.
pub async fn run_proactive_check(
    router: &ModelRouter,
    daily_budget: &DailyBudget,
    budget_limit: u64,
    context_summary: &str,
) -> anyhow::Result<Option<String>> {
    // Resolve the observer model (cheap/local).
    let provider = router
        .resolve(Some("observer"), None)
        .context("failed to resolve observer model for proactive check")?;

    // Budget pre-check against the per-check limit.
    daily_budget
        .check(budget_limit)
        .context("proactive check budget exceeded")?;

    debug!("running proactive behavior check");

    let request = CompletionRequest {
        messages: vec![Message {
            role: Role::User,
            content: MessageContent::Text(context_summary.to_owned()),
        }],
        system: Some(PROACTIVE_SYSTEM_PROMPT.to_owned()),
        tools: vec![],
        max_tokens: Some(256),
        stop_sequences: vec![],
    };

    let response = provider
        .complete(request)
        .await
        .context("proactive check LLM call failed")?;

    // Record actual token usage.
    let total = u64::from(response.usage.input_tokens)
        .saturating_add(u64::from(response.usage.output_tokens));
    daily_budget.record(total);

    // Extract response text.
    let response_text = extract_text(&response.content);

    let trimmed = response_text.trim();

    if trimmed.is_empty() || trimmed.starts_with("[NO_REPLY]") {
        info!(
            event = "proactive_check",
            action_taken = false,
            "proactive check: no action"
        );
        return Ok(None);
    }

    info!(
        event = "proactive_check",
        action_taken = true,
        "proactive check: action suggested"
    );
    Ok(Some(trimmed.to_owned()))
}
