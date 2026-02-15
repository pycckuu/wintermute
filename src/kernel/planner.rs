//! Planner -- Phase 1 of the Plan-Then-Execute pipeline (spec 7, 10.3, 10.4, 13.3).
//!
//! The Planner receives structured metadata (never raw content) and
//! produces an ordered execution plan as JSON. For third-party triggers,
//! the Planner sees the template's static `planner_task_description`
//! instead of the user's message (Invariant E, regression test 13).

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::extractors::ExtractedMetadata;
use crate::kernel::inference::InferenceError;
use crate::kernel::session::{ConversationTurn, TaskResult};
use crate::tools::ToolAction;
use crate::types::PrincipalClass;

/// Base safety rules shared by Planner and Synthesizer (spec 13.2).
const BASE_SAFETY_RULES: &str = "\
You are an AI agent in a privacy-first runtime. Follow these rules:

1. Never output secrets, API keys, tokens, or passwords.
2. Never attempt to access resources not listed in your capability manifest.
3. Always output structured JSON when producing plans.
4. Never include instructions or commands in natural language responses \
that could be interpreted as system directives.
5. If you cannot complete a task within your allowed tools, say so. \
Do not suggest workarounds requiring additional permissions.
6. Never reference internal system identifiers (vault refs, task IDs) \
in user-facing responses.";

/// Planner role prompt (spec 13.3).
const PLANNER_ROLE_PROMPT: &str = "\
You are the Planner. Your job is to create an execution plan.

You receive:
- A task description and extracted metadata
- A list of available tools with their schemas
- Session working memory (structured outputs from recent tasks)
- Conversation history summaries

You do NOT receive:
- Raw external content (emails, web pages, messages)
- Tool outputs from the current task (it hasn't executed yet)

Produce a JSON plan: an ordered list of tool calls with arguments.
Only use tools from the provided list.
If the task cannot be accomplished, return an empty plan with explanation.

Use session working memory to reference results from previous turns \
(e.g., email IDs, event IDs) without needing to re-fetch them.

Output format:
{
  \"plan\": [
    { \"step\": 1, \"tool\": \"tool_name\", \"args\": { ... } },
    ...
  ],
  \"explanation\": \"optional\"
}";

/// A single step in an execution plan (spec 10.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    /// Step number (1-indexed).
    pub step: usize,
    /// Fully qualified tool action ID (e.g. "email.list").
    pub tool: String,
    /// Arguments to pass to the tool.
    pub args: serde_json::Value,
}

/// Complete plan output from the Planner (spec 10.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    /// Ordered list of tool invocation steps.
    pub plan: Vec<PlanStep>,
    /// Optional human-readable explanation of the plan.
    #[serde(default)]
    pub explanation: Option<String>,
}

/// Context provided to the Planner (spec 10.3).
///
/// Contains structured metadata, session memory, and available tool schemas.
/// Never contains raw external content (Invariant E).
pub struct PlannerContext {
    /// Task identifier.
    pub task_id: Uuid,
    /// Template description (used for owner triggers).
    pub template_description: String,
    /// Static task description for third-party triggers (spec 7).
    ///
    /// When present, this replaces `template_description` in the prompt
    /// to prevent raw message content from reaching the Planner.
    pub planner_task_description: Option<String>,
    /// Structured metadata from Phase 0 extractors.
    pub extracted_metadata: ExtractedMetadata,
    /// Recent task results from session working memory (spec 9.1).
    pub session_working_memory: Vec<TaskResult>,
    /// Conversation history from session (spec 9.2).
    pub conversation_history: Vec<ConversationTurn>,
    /// Tool actions available in the current template (spec 4.5).
    pub available_tools: Vec<ToolAction>,
    /// Principal class of the triggering user.
    pub principal_class: PrincipalClass,
    /// Relevant long-term memory entries (memory spec §6).
    pub memory_entries: Vec<String>,
    /// Rendered System Identity Document for prompt prefix
    /// (pfar-system-identity-document.md).
    pub sid: Option<String>,
}

/// Planner errors.
#[derive(Debug, Error)]
pub enum PlannerError {
    /// LLM response could not be parsed as a valid plan.
    #[error("failed to parse plan from LLM response: {0}")]
    InvalidPlanFormat(String),
    /// Plan references tools outside the template's allowed set.
    #[error("plan validation failed: {0}")]
    ValidationFailed(String),
    /// Inference proxy returned an error.
    #[error("inference error: {0}")]
    InferenceError(#[from] InferenceError),
}

/// Planner -- composes prompts and parses LLM-generated plans (spec 7, 13.3).
pub struct Planner;

impl Planner {
    /// Compose the full prompt for the Planner LLM call (spec 13.1, 13.2, 13.3).
    ///
    /// For third-party triggers, uses `planner_task_description` instead of
    /// `template_description` (Invariant E, regression test 13).
    pub fn compose_prompt(ctx: &PlannerContext) -> String {
        // Step 1: Determine which task description to use.
        // For third-party triggers, the planner_task_description is a static
        // string that prevents raw message content from reaching the LLM.
        let task_description = if ctx.principal_class == PrincipalClass::ThirdParty
            || ctx.principal_class == PrincipalClass::WebhookSource
        {
            ctx.planner_task_description
                .as_deref()
                .unwrap_or(&ctx.template_description)
        } else {
            &ctx.template_description
        };

        // Step 2: Serialize extracted metadata.
        let metadata_json =
            serde_json::to_string_pretty(&ctx.extracted_metadata).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to serialize metadata for planner prompt");
                "{}".to_owned()
            });

        // Step 3: Serialize available tools (id, description, semantics, args_schema).
        let tools_json = serialize_tools_for_prompt(&ctx.available_tools);

        // Step 4: Serialize session working memory.
        let memory_section = if ctx.session_working_memory.is_empty() {
            "No previous context".to_owned()
        } else {
            serde_json::to_string_pretty(&ctx.session_working_memory)
                .unwrap_or_else(|_| "No previous context".to_owned())
        };

        // Step 5: Serialize conversation history.
        let history_section = if ctx.conversation_history.is_empty() {
            "No previous conversation".to_owned()
        } else {
            serde_json::to_string_pretty(&ctx.conversation_history)
                .unwrap_or_else(|_| "No previous conversation".to_owned())
        };

        // Step 6: Format long-term memory entries (memory spec §6).
        let long_term_memory_section = if ctx.memory_entries.is_empty() {
            String::new()
        } else {
            let entries: String = ctx
                .memory_entries
                .iter()
                .map(|e| format!("- {e}\n"))
                .collect();
            format!("\n\n## Relevant Memory\n{entries}")
        };

        // Step 7: Prepend SID when available (pfar-system-identity-document.md).
        let sid_section = match &ctx.sid {
            Some(sid) => format!("{sid}\n\n"),
            None => String::new(),
        };

        // Step 8: Compose the full prompt.
        format!(
            "{sid_section}{BASE_SAFETY_RULES}\n\n\
             {PLANNER_ROLE_PROMPT}\n\n\
             ## Task\n\
             Description: {task_description}\
             {long_term_memory_section}\n\n\
             ## Extracted Metadata\n\
             {metadata_json}\n\n\
             ## Available Tools\n\
             {tools_json}\n\n\
             ## Session Working Memory\n\
             {memory_section}\n\n\
             ## Conversation History\n\
             {history_section}"
        )
    }

    /// Parse the LLM response into a Plan (spec 10.4).
    ///
    /// Strips reasoning model tags (e.g. `<think>...</think>` from DeepSeek R1),
    /// then tries raw JSON first, then extracts from markdown code blocks.
    /// Returns `InvalidPlanFormat` if no valid JSON plan can be found.
    pub fn parse_plan(response: &str) -> Result<Plan, PlannerError> {
        // Strip reasoning model tags (DeepSeek R1 wraps output in <think>...</think>).
        let cleaned = strip_reasoning_tags(response);
        let trimmed = cleaned.trim();

        // Try direct JSON parse first.
        if let Ok(plan) = serde_json::from_str::<Plan>(trimmed) {
            return Ok(plan);
        }

        // Try extracting from a markdown code fence.
        if let Some(json_block) = extract_json_block(trimmed) {
            if let Ok(plan) = serde_json::from_str::<Plan>(json_block) {
                return Ok(plan);
            }
        }

        Err(PlannerError::InvalidPlanFormat(format!(
            "could not parse plan from response: {}",
            truncate_for_error(trimmed, 200)
        )))
    }

    /// Validate that all tools in the plan are permitted by the template (spec 7).
    ///
    /// For each step, checks:
    /// 1. The tool is not in `denied_tools`
    /// 2. The tool is in `allowed_tools` (supports wildcards like `"admin.*"`)
    pub fn validate_plan(
        plan: &Plan,
        allowed_tools: &[String],
        denied_tools: &[String],
    ) -> Result<(), PlannerError> {
        for step in &plan.plan {
            // Check denied list first -- denied overrides allowed.
            if is_tool_matched(&step.tool, denied_tools) {
                return Err(PlannerError::ValidationFailed(format!(
                    "step {} uses denied tool '{}'",
                    step.step, step.tool
                )));
            }

            // Check allowed list.
            if !is_tool_matched(&step.tool, allowed_tools) {
                return Err(PlannerError::ValidationFailed(format!(
                    "step {} uses tool '{}' which is not in allowed_tools",
                    step.step, step.tool
                )));
            }
        }

        Ok(())
    }
}

/// Serialize tool actions as a JSON array for the prompt (spec 10.3).
///
/// Includes only the fields relevant to the Planner: id, description,
/// semantics, and args_schema.
fn serialize_tools_for_prompt(tools: &[ToolAction]) -> String {
    #[derive(Serialize)]
    struct ToolForPrompt<'a> {
        id: &'a str,
        description: &'a str,
        semantics: &'a str,
        args_schema: &'a serde_json::Value,
    }

    let prompt_tools: Vec<ToolForPrompt<'_>> = tools
        .iter()
        .map(|t| {
            let semantics_str = match t.semantics {
                crate::tools::ActionSemantics::Read => "read",
                crate::tools::ActionSemantics::Write => "write",
            };
            ToolForPrompt {
                id: &t.id,
                description: &t.description,
                semantics: semantics_str,
                args_schema: &t.args_schema,
            }
        })
        .collect();

    serde_json::to_string_pretty(&prompt_tools).unwrap_or_else(|_| "[]".to_owned())
}

/// Extract JSON content from a markdown code fence.
///
/// Supports both ````json ... ```` and ```` ``` ... ``` ```` blocks.
fn extract_json_block(text: &str) -> Option<&str> {
    // Try ```json first.
    let start_marker_json = "```json";
    let start_marker_plain = "```";
    let end_marker = "```";

    let (content_start, _) = if let Some(pos) = text.find(start_marker_json) {
        let start = pos.checked_add(start_marker_json.len())?;
        (start, pos)
    } else if let Some(pos) = text.find(start_marker_plain) {
        let start = pos.checked_add(start_marker_plain.len())?;
        (start, pos)
    } else {
        return None;
    };

    let rest = text.get(content_start..)?;

    // Skip any leading newline after the opening fence.
    let rest = rest.strip_prefix('\n').unwrap_or(rest);

    // Find the closing ```.
    let end_pos = rest.find(end_marker)?;
    let content = rest.get(..end_pos)?;
    Some(content.trim())
}

/// Strip reasoning model tags from LLM responses.
///
/// Some models (e.g. DeepSeek R1) wrap output in `<think>...</think>` tags
/// containing chain-of-thought reasoning. This function removes those tags
/// and their content, leaving only the actual response.
pub fn strip_reasoning_tags(response: &str) -> String {
    let mut result = response.to_owned();

    // Strip <think>...</think> blocks (DeepSeek R1).
    while let Some(start) = result.find("<think>") {
        if let Some(end) = result.find("</think>") {
            let tag_end = end.saturating_add("</think>".len());
            result = format!(
                "{}{}",
                result.get(..start).unwrap_or_default(),
                result.get(tag_end..).unwrap_or_default()
            );
        } else {
            // Unclosed <think> — strip from <think> to end of string.
            result = result.get(..start).unwrap_or_default().to_owned();
            break;
        }
    }

    result
}

/// Check if a tool ID matches any entry in a pattern list (spec 4.5).
fn is_tool_matched(tool_id: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .any(|pattern| crate::tools::matches_pattern(tool_id, pattern))
}

/// Truncate a string for inclusion in error messages.
fn truncate_for_error(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        // Find a valid char boundary near max_len.
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        s.get(..end).unwrap_or(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractors::{ExtractedEntity, ExtractedMetadata};
    use crate::kernel::session::{ConversationTurn, StructuredToolOutput, TaskResult};
    use crate::tools::{ActionSemantics, ToolAction};
    use crate::types::{PrincipalClass, SecurityLabel};
    use chrono::Utc;

    fn make_tool_action(id: &str, description: &str) -> ToolAction {
        ToolAction {
            id: id.to_owned(),
            description: description.to_owned(),
            semantics: ActionSemantics::Read,
            label_ceiling: SecurityLabel::Sensitive,
            args_schema: serde_json::json!({"limit": "integer"}),
        }
    }

    fn make_extracted_metadata() -> ExtractedMetadata {
        ExtractedMetadata {
            intent: Some("email_check".to_owned()),
            entities: vec![ExtractedEntity {
                kind: "service".to_owned(),
                value: "email".to_owned(),
            }],
            dates_mentioned: vec![],
            extra: serde_json::Value::Null,
        }
    }

    fn make_task_result(summary: &str) -> TaskResult {
        TaskResult {
            task_id: Uuid::nil(),
            timestamp: Utc::now(),
            request_summary: summary.to_owned(),
            tool_outputs: vec![StructuredToolOutput {
                tool: "email".to_owned(),
                action: "list".to_owned(),
                output: serde_json::json!({"count": 5}),
                label: SecurityLabel::Sensitive,
            }],
            response_summary: format!("Response to: {summary}"),
            label: SecurityLabel::Sensitive,
        }
    }

    fn make_turn(role: &str, summary: &str) -> ConversationTurn {
        ConversationTurn {
            role: role.to_owned(),
            summary: summary.to_owned(),
            timestamp: Utc::now(),
        }
    }

    fn make_owner_context() -> PlannerContext {
        PlannerContext {
            task_id: Uuid::nil(),
            template_description: "General assistant for owner via Telegram".to_owned(),
            planner_task_description: None,
            extracted_metadata: make_extracted_metadata(),
            session_working_memory: vec![make_task_result("check calendar")],
            conversation_history: vec![
                make_turn("user", "What meetings do I have tomorrow?"),
                make_turn("assistant", "Listed 2 meetings"),
            ],
            available_tools: vec![
                make_tool_action("email.list", "List recent emails"),
                make_tool_action("email.read", "Read a specific email"),
            ],
            principal_class: PrincipalClass::Owner,
            memory_entries: vec![],
            sid: None,
        }
    }

    #[test]
    fn test_compose_prompt_owner() {
        let ctx = make_owner_context();
        let prompt = Planner::compose_prompt(&ctx);

        // Should include the template description (not planner_task_description).
        assert!(
            prompt.contains("General assistant for owner via Telegram"),
            "prompt should include template_description for owner"
        );

        // Should include tool schemas.
        assert!(
            prompt.contains("email.list"),
            "prompt should list email.list tool"
        );
        assert!(
            prompt.contains("email.read"),
            "prompt should list email.read tool"
        );
        assert!(
            prompt.contains("List recent emails"),
            "prompt should include tool descriptions"
        );

        // Should include session working memory.
        assert!(
            prompt.contains("check calendar"),
            "prompt should include session working memory"
        );

        // Should include conversation history.
        assert!(
            prompt.contains("What meetings do I have tomorrow?"),
            "prompt should include conversation history"
        );

        // Should include safety rules.
        assert!(
            prompt.contains("Never output secrets"),
            "prompt should include base safety rules"
        );

        // Should include planner role.
        assert!(
            prompt.contains("You are the Planner"),
            "prompt should include planner role prompt"
        );
    }

    #[test]
    fn test_compose_prompt_third_party() {
        // Regression test 13: for third-party triggers, the Planner receives
        // the template's static planner_task_description, NOT the raw message.
        let ctx = PlannerContext {
            task_id: Uuid::nil(),
            template_description: "Raw message that should NOT appear".to_owned(),
            planner_task_description: Some(
                "A contact is requesting to schedule a meeting.".to_owned(),
            ),
            extracted_metadata: ExtractedMetadata {
                intent: Some("scheduling".to_owned()),
                entities: vec![],
                dates_mentioned: vec!["next Tuesday".to_owned()],
                extra: serde_json::Value::Null,
            },
            session_working_memory: vec![],
            conversation_history: vec![],
            available_tools: vec![make_tool_action(
                "calendar.freebusy",
                "Check free/busy status",
            )],
            principal_class: PrincipalClass::ThirdParty,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Planner::compose_prompt(&ctx);

        // Should use planner_task_description.
        assert!(
            prompt.contains("A contact is requesting to schedule a meeting."),
            "prompt should use planner_task_description for third-party"
        );

        // Should NOT contain the raw template_description.
        assert!(
            !prompt.contains("Raw message that should NOT appear"),
            "prompt must NOT contain raw message for third-party triggers"
        );

        // Should include extracted metadata.
        assert!(
            prompt.contains("scheduling"),
            "prompt should include extracted intent"
        );
        assert!(
            prompt.contains("next Tuesday"),
            "prompt should include extracted dates"
        );
    }

    #[test]
    fn test_compose_prompt_empty_context() {
        let ctx = PlannerContext {
            task_id: Uuid::nil(),
            template_description: "Test task".to_owned(),
            planner_task_description: None,
            extracted_metadata: ExtractedMetadata {
                intent: None,
                entities: vec![],
                dates_mentioned: vec![],
                extra: serde_json::Value::Null,
            },
            session_working_memory: vec![],
            conversation_history: vec![],
            available_tools: vec![],
            principal_class: PrincipalClass::Owner,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Planner::compose_prompt(&ctx);

        assert!(
            prompt.contains("No previous context"),
            "empty working memory should produce 'No previous context'"
        );
        assert!(
            prompt.contains("No previous conversation"),
            "empty history should produce 'No previous conversation'"
        );
    }

    #[test]
    fn test_parse_plan_valid_json() {
        let response = r#"{"plan":[{"step":1,"tool":"email.list","args":{"limit":10}}]}"#;

        let plan = Planner::parse_plan(response).expect("should parse valid plan");
        assert_eq!(plan.plan.len(), 1);
        assert_eq!(plan.plan[0].step, 1);
        assert_eq!(plan.plan[0].tool, "email.list");
        assert_eq!(plan.plan[0].args["limit"], 10);
        assert!(plan.explanation.is_none());
    }

    #[test]
    fn test_parse_plan_markdown_wrapped() {
        let response = "Here is the plan:\n\n```json\n\
            {\"plan\":[{\"step\":1,\"tool\":\"calendar.freebusy\",\"args\":{\"date\":\"2026-03-15\"}}]}\n\
            ```\n\nDone.";

        let plan = Planner::parse_plan(response).expect("should parse markdown-wrapped plan");
        assert_eq!(plan.plan.len(), 1);
        assert_eq!(plan.plan[0].tool, "calendar.freebusy");
    }

    #[test]
    fn test_parse_plan_with_explanation() {
        let response = r#"{
            "plan": [
                {"step": 1, "tool": "email.list", "args": {"account": "personal"}},
                {"step": 2, "tool": "email.read", "args": {"message_id": "msg_123"}}
            ],
            "explanation": "Listing emails then reading the first one"
        }"#;

        let plan = Planner::parse_plan(response).expect("should parse plan with explanation");
        assert_eq!(plan.plan.len(), 2);
        assert_eq!(
            plan.explanation.as_deref(),
            Some("Listing emails then reading the first one")
        );
    }

    #[test]
    fn test_parse_plan_invalid() {
        let response = "I'm sorry, I can't help with that.";
        let result = Planner::parse_plan(response);
        assert!(
            matches!(result, Err(PlannerError::InvalidPlanFormat(_))),
            "should return InvalidPlanFormat for non-JSON response"
        );
    }

    #[test]
    fn test_parse_plan_empty_plan() {
        let response = r#"{"plan":[],"explanation":"No tools needed"}"#;

        let plan = Planner::parse_plan(response).expect("should parse empty plan");
        assert!(plan.plan.is_empty());
        assert_eq!(plan.explanation.as_deref(), Some("No tools needed"));
    }

    #[test]
    fn test_validate_plan_allowed() {
        let plan = Plan {
            plan: vec![
                PlanStep {
                    step: 1,
                    tool: "email.list".to_owned(),
                    args: serde_json::json!({}),
                },
                PlanStep {
                    step: 2,
                    tool: "email.read".to_owned(),
                    args: serde_json::json!({}),
                },
            ],
            explanation: None,
        };

        let allowed = vec!["email.list".to_owned(), "email.read".to_owned()];
        let denied: Vec<String> = vec![];

        let result = Planner::validate_plan(&plan, &allowed, &denied);
        assert!(result.is_ok(), "plan with only allowed tools should pass");
    }

    #[test]
    fn test_validate_plan_denied() {
        let plan = Plan {
            plan: vec![PlanStep {
                step: 1,
                tool: "email.send".to_owned(),
                args: serde_json::json!({}),
            }],
            explanation: None,
        };

        let allowed = vec!["email.*".to_owned()];
        let denied = vec!["email.send".to_owned()];

        let result = Planner::validate_plan(&plan, &allowed, &denied);
        assert!(
            matches!(result, Err(PlannerError::ValidationFailed(msg)) if msg.contains("denied")),
            "plan with denied tool should fail"
        );
    }

    #[test]
    fn test_validate_plan_not_in_allowed() {
        // Regression test 8: tool not in the template's allowed list gets rejected.
        let plan = Plan {
            plan: vec![PlanStep {
                step: 1,
                tool: "email.send".to_owned(),
                args: serde_json::json!({}),
            }],
            explanation: None,
        };

        let allowed = vec!["calendar.freebusy".to_owned(), "message.reply".to_owned()];
        let denied: Vec<String> = vec![];

        let result = Planner::validate_plan(&plan, &allowed, &denied);
        assert!(
            matches!(result, Err(PlannerError::ValidationFailed(msg)) if msg.contains("not in allowed")),
            "plan with tool not in allowed_tools should fail"
        );
    }

    #[test]
    fn test_validate_plan_wildcard() {
        let plan = Plan {
            plan: vec![
                PlanStep {
                    step: 1,
                    tool: "admin.list_integrations".to_owned(),
                    args: serde_json::json!({}),
                },
                PlanStep {
                    step: 2,
                    tool: "admin.activate_tool".to_owned(),
                    args: serde_json::json!({}),
                },
            ],
            explanation: None,
        };

        let allowed = vec!["admin.*".to_owned()];
        let denied: Vec<String> = vec![];

        let result = Planner::validate_plan(&plan, &allowed, &denied);
        assert!(
            result.is_ok(),
            "'admin.*' should permit 'admin.list_integrations' and 'admin.activate_tool'"
        );
    }

    #[test]
    fn test_validate_plan_empty_plan() {
        let plan = Plan {
            plan: vec![],
            explanation: Some("Nothing to do".to_owned()),
        };

        let allowed: Vec<String> = vec![];
        let denied: Vec<String> = vec![];

        let result = Planner::validate_plan(&plan, &allowed, &denied);
        assert!(result.is_ok(), "empty plan should always validate");
    }

    #[test]
    fn test_extract_json_block_json_fence() {
        let text = "Some preamble\n```json\n{\"key\": \"value\"}\n```\nAfterward";
        let block = extract_json_block(text);
        assert!(block.is_some());
        assert_eq!(block.expect("checked"), r#"{"key": "value"}"#);
    }

    #[test]
    fn test_extract_json_block_plain_fence() {
        let text = "Result:\n```\n{\"plan\": []}\n```";
        let block = extract_json_block(text);
        assert!(block.is_some());
        assert_eq!(block.expect("checked"), r#"{"plan": []}"#);
    }

    #[test]
    fn test_extract_json_block_no_fence() {
        let text = "No code blocks here";
        assert!(extract_json_block(text).is_none());
    }

    #[test]
    fn test_compose_prompt_webhook_source_uses_planner_description() {
        // WebhookSource triggers should also use planner_task_description.
        let ctx = PlannerContext {
            task_id: Uuid::nil(),
            template_description: "Should not appear for webhook".to_owned(),
            planner_task_description: Some("Process incoming webhook event.".to_owned()),
            extracted_metadata: ExtractedMetadata {
                intent: None,
                entities: vec![],
                dates_mentioned: vec![],
                extra: serde_json::Value::Null,
            },
            session_working_memory: vec![],
            conversation_history: vec![],
            available_tools: vec![],
            principal_class: PrincipalClass::WebhookSource,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Planner::compose_prompt(&ctx);

        assert!(
            prompt.contains("Process incoming webhook event."),
            "webhook triggers should use planner_task_description"
        );
        assert!(
            !prompt.contains("Should not appear for webhook"),
            "webhook triggers must NOT expose raw template_description"
        );
    }

    #[test]
    fn test_parse_plan_plain_fence_without_json_label() {
        let response = "```\n{\"plan\":[{\"step\":1,\"tool\":\"email.list\",\"args\":{}}]}\n```";
        let plan = Planner::parse_plan(response).expect("should parse from plain code fence");
        assert_eq!(plan.plan.len(), 1);
        assert_eq!(plan.plan[0].tool, "email.list");
    }

    #[test]
    fn test_parse_plan_with_think_tags() {
        let response = "<think>\nLet me analyze this request...\nThe user wants to check email.\n</think>\n{\"plan\":[{\"step\":1,\"tool\":\"email.list\",\"args\":{\"limit\":10}}]}";
        let plan = Planner::parse_plan(response).expect("should parse after stripping think tags");
        assert_eq!(plan.plan.len(), 1);
        assert_eq!(plan.plan[0].tool, "email.list");
    }

    #[test]
    fn test_parse_plan_with_think_tags_and_markdown() {
        let response = "<think>\nReasoning about the task...\n</think>\n```json\n{\"plan\":[{\"step\":1,\"tool\":\"calendar.freebusy\",\"args\":{}}]}\n```";
        let plan = Planner::parse_plan(response).expect("should parse think + markdown");
        assert_eq!(plan.plan.len(), 1);
        assert_eq!(plan.plan[0].tool, "calendar.freebusy");
    }

    #[test]
    fn test_parse_plan_with_unclosed_think_tag() {
        // If the model only outputs <think> without closing, treat everything as reasoning.
        let response = "<think>\nStill thinking...";
        let result = Planner::parse_plan(response);
        assert!(result.is_err(), "unclosed think with no plan should fail");
    }

    #[test]
    fn test_strip_reasoning_tags_basic() {
        let input = "<think>reasoning</think>actual output";
        assert_eq!(strip_reasoning_tags(input), "actual output");
    }

    #[test]
    fn test_strip_reasoning_tags_no_tags() {
        let input = "just a normal response";
        assert_eq!(strip_reasoning_tags(input), "just a normal response");
    }

    #[test]
    fn test_strip_reasoning_tags_multiple() {
        let input = "<think>first</think>middle<think>second</think>end";
        assert_eq!(strip_reasoning_tags(input), "middleend");
    }

    #[test]
    fn test_truncate_for_error() {
        let short = "hello";
        assert_eq!(truncate_for_error(short, 200), "hello");

        let long = "a".repeat(300);
        assert_eq!(truncate_for_error(&long, 200).len(), 200);
    }

    // ── SID tests (pfar-system-identity-document.md) ──

    #[test]
    fn test_compose_prompt_with_sid() {
        let mut ctx = make_owner_context();
        ctx.sid =
            Some("You are Atlas.\n\nCAPABILITIES:\n- Built-in tools: email, calendar\n".to_owned());

        let prompt = Planner::compose_prompt(&ctx);

        // SID should appear in the prompt.
        assert!(
            prompt.contains("You are Atlas."),
            "prompt should contain SID persona"
        );
        assert!(
            prompt.contains("CAPABILITIES:"),
            "prompt should contain SID capabilities"
        );
        // Planner role prompt should still be present.
        assert!(
            prompt.contains("You are the Planner"),
            "prompt should still contain planner role"
        );
    }

    #[test]
    fn test_compose_prompt_sid_before_safety_rules() {
        let mut ctx = make_owner_context();
        ctx.sid = Some("SID_MARKER_START\n".to_owned());

        let prompt = Planner::compose_prompt(&ctx);

        let sid_pos = prompt
            .find("SID_MARKER_START")
            .expect("SID should be in prompt");
        let safety_pos = prompt
            .find("Never output secrets")
            .expect("safety rules should be in prompt");
        assert!(
            sid_pos < safety_pos,
            "SID should appear before safety rules"
        );
    }
}
