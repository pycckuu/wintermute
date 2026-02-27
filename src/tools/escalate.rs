//! Escalate tool: consult a more powerful "oracle" model for hard problems.
//!
//! The agent calls `escalate` when it encounters a problem beyond its current
//! model's capability. The tool resolves the `oracle` role from [`ModelRouter`]
//! and sends a focused question, returning the oracle's answer.

use tracing::info;

use crate::agent::budget::DailyBudget;
use crate::providers::router::ModelRouter;
use crate::providers::{
    extract_text, CompletionRequest, Message, MessageContent, Role, ToolDefinition,
};

use super::ToolError;

/// Estimated tokens per escalation call (for budget pre-check).
const ESTIMATED_ESCALATION_TOKENS: u64 = 2000;

/// System prompt for the oracle model.
const ORACLE_SYSTEM_PROMPT: &str =
    "You are an expert consultant. Answer the question directly and concisely.";

/// Execute the escalate tool: ask a more powerful model for help.
///
/// Resolves the `oracle` role from the model router. If no oracle is configured,
/// falls back to the default model but warns the user.
///
/// # Errors
///
/// Returns [`ToolError::ExecutionFailed`] if the provider call fails or budget
/// is exceeded. Returns [`ToolError::InvalidInput`] if `question` is missing.
pub async fn escalate(
    router: &ModelRouter,
    daily_budget: &DailyBudget,
    input: &serde_json::Value,
) -> Result<String, ToolError> {
    let question = input
        .get("question")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("escalate requires a 'question' field".into()))?;

    let context = input.get("context").and_then(|v| v.as_str()).unwrap_or("");

    // Resolve oracle model (falls back to default if no oracle role configured).
    let provider = router
        .resolve(Some("oracle"), None)
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to resolve oracle model: {e}")))?;

    // Budget pre-check.
    daily_budget
        .check(ESTIMATED_ESCALATION_TOKENS)
        .map_err(|e| ToolError::ExecutionFailed(format!("budget exceeded for escalation: {e}")))?;

    // Build the user message with question and optional context.
    let mut user_text = question.to_owned();
    if !context.is_empty() {
        user_text.push_str("\n\nContext:\n");
        user_text.push_str(context);
    }

    let request = CompletionRequest {
        messages: vec![Message {
            role: Role::User,
            content: MessageContent::Text(user_text),
        }],
        system: Some(ORACLE_SYSTEM_PROMPT.to_owned()),
        tools: vec![],
        max_tokens: Some(4096),
        stop_sequences: vec![],
    };

    info!(
        model = %provider.model_id(),
        event = "escalation",
        "escalating to oracle model"
    );

    let response = provider
        .complete(request)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("oracle model call failed: {e}")))?;

    // Record actual token usage.
    let total_tokens = u64::from(response.usage.input_tokens)
        .saturating_add(u64::from(response.usage.output_tokens));
    daily_budget.record(total_tokens);

    info!(
        event = "escalation",
        model = %response.model,
        tokens_used = total_tokens,
        "escalation complete"
    );

    // Extract text from response.
    let answer = extract_text(&response.content);

    if answer.is_empty() {
        return Err(ToolError::ExecutionFailed(
            "oracle returned empty response".into(),
        ));
    }

    Ok(format!(
        "[Oracle ({model}, {total_tokens} tokens)]\n{answer}",
        model = response.model
    ))
}

/// Return the tool definition for `escalate`.
pub fn escalate_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "escalate".to_owned(),
        description: "Ask a more powerful model (oracle) for help with a difficult problem. \
            Use when you're stuck, uncertain, or the task requires deeper expertise."
            .to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The specific question or problem to escalate."
                },
                "context": {
                    "type": "string",
                    "description": "Optional additional context (code snippets, error messages, etc.)."
                }
            },
            "required": ["question"]
        }),
    }
}
