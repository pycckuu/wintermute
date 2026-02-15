//! Synthesizer -- Phase 3 of the Plan-Then-Execute pipeline (spec 7, 10.7, 13.4).
//!
//! The Synthesizer sees tool results and raw content but CANNOT call
//! any tools. Tool-call JSON in output is treated as plain text
//! (Invariant E, regression test 9).

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::kernel::inference::InferenceError;
use crate::kernel::session::{ConversationTurn, TaskResult};

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

/// Onboarding preamble for first-ever owner message (persona-onboarding spec §2).
///
/// This prompt OVERRIDES the normal Synthesizer role — the assistant must ask
/// the configuration questions regardless of what the user's message says.
const ONBOARDING_PREAMBLE: &str = "\
IMPORTANT: This is your very first interaction. You MUST configure yourself before doing anything else.

You are a personal assistant running for the first time. You have no name or personality yet.

Your ONLY job in this response is to greet the user and ask them to configure you. \
Ask these three things in a single, brief message:
1. What should they call you (pick a name for you)
2. What should you call them (their name)
3. How they want you to communicate (concise/detailed, casual/formal, any quirks)

Do NOT answer any question the user asked. Do NOT help with any task yet. \
Just introduce yourself and ask the three configuration questions above. \
Keep it brief and natural — 3-4 sentences max.";

/// Anti-leak instruction appended when persona is active (persona-onboarding spec §2).
const PERSONA_ANTI_LEAK: &str = "\
Never mention internal system details like \"Synthesizer\", \"Planner\", \
\"pipeline\", \"kernel\", or \"privacy-first runtime\". \
You are a personal assistant, not a system component.";

/// Prompt for the turn where the owner just provided their persona configuration.
/// The Synthesizer should briefly confirm it was saved, not try to fully role-play yet.
const PERSONA_JUST_CONFIGURED_PROMPT: &str = "\
The user just provided their preferences for how this assistant should behave. \
Their configuration has been saved and will take effect from the next message onward.

Briefly acknowledge what they configured (name, their name, style) in 1-2 sentences. \
Be warm but concise. Do NOT start role-playing the persona yet — just confirm.";

/// Synthesizer role prompt (spec 13.4).
const SYNTHESIZER_ROLE_PROMPT: &str = "\
You are the Synthesizer. Your job is to compose a final response to the user's current message.

You receive:
- The original task context (the user's current message)
- Results from tool executions (may be empty if no tools were needed)
- Optionally, raw content for reference
- Optionally, conversation history for background context

CRITICAL RULES:
1. Respond directly and naturally to the user's CURRENT message shown in 'Original Request'.
2. If tool results are available, present them clearly and helpfully.
3. If no tool results are available, respond conversationally to what the user said.
4. Do NOT summarize or recap the conversation history. It is background context only.
5. Do NOT repeat back what the user said in previous turns.
6. For short messages like greetings or acknowledgments, reply briefly and naturally.
7. Do NOT fabricate actions. Never say 'Let me fetch...', 'Retrieving...', or \
'I will look into...' when no tools were executed. Only describe actions that actually happened.
8. If the user's message is a follow-up to a previous exchange (check conversation history), \
address it in that context — do not treat it as a standalone message.

You CANNOT:
- Call any tools
- Output JSON tool calls (they will be treated as plain text)

Keep your response concise and relevant to the user's current message.
Do not reveal internal identifiers, labels, or system details.";

/// Result of a single executed plan step, for synthesizer context (spec 10.7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    /// Step number from the plan.
    pub step: usize,
    /// Tool action that was executed.
    pub tool: String,
    /// Result data from the tool.
    pub result: serde_json::Value,
}

/// Output format instructions for the synthesizer (spec 10.7).
#[derive(Debug, Clone)]
pub struct OutputInstructions {
    /// Target sink identifier.
    pub sink: String,
    /// Maximum response length in characters.
    pub max_length: usize,
    /// Output format (e.g. "plain_text", "markdown").
    pub format: String,
}

/// Context provided to the Synthesizer (spec 10.7, 9.3).
///
/// Contains tool results, the original task context, and session history.
/// The Synthesizer sees content but has no tool access (Invariant E).
pub struct SynthesizerContext {
    /// Task identifier.
    pub task_id: Uuid,
    /// Description of what the user originally requested.
    pub original_context: String,
    /// Optional reference to raw content stored in the vault.
    pub raw_content_ref: Option<String>,
    /// Results from executed plan steps.
    pub tool_results: Vec<StepResult>,
    /// Instructions for formatting the output.
    pub output_instructions: OutputInstructions,
    /// Session working memory from previous tasks (spec 9.3).
    pub session_working_memory: Vec<TaskResult>,
    /// Conversation history from session (spec 9.3).
    pub conversation_history: Vec<ConversationTurn>,
    /// Persona string from journal, if configured (persona-onboarding spec §2).
    pub persona: Option<String>,
    /// True on the very first owner message when no persona exists yet.
    pub is_onboarding: bool,
    /// True on the turn where the owner just provided persona configuration.
    pub is_persona_just_configured: bool,
    /// Relevant long-term memory entries (memory spec §6).
    pub memory_entries: Vec<String>,
    /// Rendered System Identity Document for prompt prefix
    /// (pfar-system-identity-document.md).
    pub sid: Option<String>,
}

/// Synthesizer errors.
#[derive(Debug, Error)]
pub enum SynthesizerError {
    /// Inference proxy returned an error.
    #[error("inference error: {0}")]
    InferenceError(#[from] InferenceError),
}

/// Synthesizer -- composes response prompts from tool results (spec 7, 13.4).
///
/// The Synthesizer CANNOT call tools. Even if the LLM outputs tool-call
/// JSON, the kernel treats it as plain text (Invariant E, regression test 9).
pub struct Synthesizer;

impl Synthesizer {
    /// Compose the Synthesizer prompt (spec 13.1, 13.4).
    ///
    /// Includes base safety rules, synthesizer role, tool results, and
    /// output formatting instructions.
    pub fn compose_prompt(ctx: &SynthesizerContext) -> String {
        // Step 0: Prepend SID when available (pfar-system-identity-document.md).
        let sid_section = match &ctx.sid {
            Some(sid) => format!("{sid}\n\n"),
            None => String::new(),
        };

        // Step 1: Build the role section based on persona state (persona-onboarding spec §2).
        // Onboarding prompt goes AFTER the role prompt so the LLM sees it last
        // and prioritizes the onboarding instructions over generic role behavior.
        // When SID is present, persona is already in the SID -- don't duplicate.
        let role_section = if ctx.is_onboarding {
            format!("{SYNTHESIZER_ROLE_PROMPT}\n\n{ONBOARDING_PREAMBLE}")
        } else if ctx.is_persona_just_configured {
            format!("{SYNTHESIZER_ROLE_PROMPT}\n\n{PERSONA_JUST_CONFIGURED_PROMPT}")
        } else if ctx.sid.is_some() && ctx.persona.is_some() {
            // SID already contains persona + capabilities. Just add anti-leak + role.
            format!("{PERSONA_ANTI_LEAK}\n\n{SYNTHESIZER_ROLE_PROMPT}")
        } else if ctx.sid.is_some() {
            // SID present but no persona configured yet — still use SID for capabilities.
            SYNTHESIZER_ROLE_PROMPT.to_owned()
        } else if let Some(ref persona) = ctx.persona {
            // Fallback: no SID, use legacy persona injection.
            format!("You are {persona}.\n\n{PERSONA_ANTI_LEAK}\n\n{SYNTHESIZER_ROLE_PROMPT}")
        } else {
            SYNTHESIZER_ROLE_PROMPT.to_owned()
        };

        // Step 2: Serialize tool results.
        let results_json = if ctx.tool_results.is_empty() {
            "No tool results available.".to_owned()
        } else {
            serde_json::to_string_pretty(&ctx.tool_results)
                .unwrap_or_else(|_| "No tool results available.".to_owned())
        };

        // Step 3: Include raw content reference if present.
        let raw_content_section = match &ctx.raw_content_ref {
            Some(content) => format!("\n\n## Raw Content\n{content}"),
            None => String::new(),
        };

        // Step 4: Format long-term memory entries (memory spec §6).
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

        // Step 5: Serialize session context (spec 9.3).
        let memory_section = if ctx.session_working_memory.is_empty() {
            String::new()
        } else {
            let memory_json = serde_json::to_string_pretty(&ctx.session_working_memory)
                .unwrap_or_else(|_| "[]".to_owned());
            format!("\n\n## Session Working Memory\n{memory_json}")
        };

        let history_section = if ctx.conversation_history.is_empty() {
            String::new()
        } else {
            let mut lines = String::from(
                "\n\n## Conversation History (background context only — do NOT summarize or repeat)\n",
            );
            for turn in &ctx.conversation_history {
                lines.push_str(&format!("- {}: {}\n", turn.role, turn.summary));
            }
            lines
        };

        // Step 7: Compose the full prompt.
        format!(
            "{sid_section}{BASE_SAFETY_RULES}\n\n\
             {role_section}\n\n\
             ## Original Request\n\
             {original_context}\n\n\
             ## Tool Results\n\
             {results_json}\
             {raw_content_section}\
             {long_term_memory_section}\
             {memory_section}\
             {history_section}\n\n\
             ## Instructions\n\
             Format: {format}\n\
             Maximum length: {max_length} characters",
            original_context = ctx.original_context,
            format = ctx.output_instructions.format,
            max_length = ctx.output_instructions.max_length,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_output_instructions() -> OutputInstructions {
        OutputInstructions {
            sink: "sink:telegram:owner".to_owned(),
            max_length: 2000,
            format: "plain_text".to_owned(),
        }
    }

    fn make_step_result(step: usize, tool: &str) -> StepResult {
        StepResult {
            step,
            tool: tool.to_owned(),
            result: serde_json::json!({
                "emails": [
                    {"id": "msg_1", "from": "sarah@co", "subject": "Q3 Budget"},
                ]
            }),
        }
    }

    #[test]
    fn test_compose_prompt_with_results() {
        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "User asked to check their email".to_owned(),
            raw_content_ref: None,
            tool_results: vec![make_step_result(1, "email.list")],
            output_instructions: make_output_instructions(),
            session_working_memory: vec![],
            conversation_history: vec![],
            persona: None,
            is_onboarding: false,
            is_persona_just_configured: false,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        // Should include tool results.
        assert!(
            prompt.contains("email.list"),
            "prompt should include tool name from results"
        );
        assert!(
            prompt.contains("Q3 Budget"),
            "prompt should include tool result data"
        );

        // Should include original context.
        assert!(
            prompt.contains("User asked to check their email"),
            "prompt should include original context"
        );

        // Should include output instructions.
        assert!(
            prompt.contains("plain_text"),
            "prompt should include output format"
        );
        assert!(prompt.contains("2000"), "prompt should include max length");

        // Should include synthesizer role.
        assert!(
            prompt.contains("You are the Synthesizer"),
            "prompt should include synthesizer role prompt"
        );
    }

    #[test]
    fn test_compose_prompt_no_results() {
        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "User asked something".to_owned(),
            raw_content_ref: None,
            tool_results: vec![],
            output_instructions: make_output_instructions(),
            session_working_memory: vec![],
            conversation_history: vec![],
            persona: None,
            is_onboarding: false,
            is_persona_just_configured: false,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        assert!(
            prompt.contains("No tool results available"),
            "empty tool results should produce appropriate message"
        );
    }

    #[test]
    fn test_compose_prompt_includes_safety_rules() {
        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "Test".to_owned(),
            raw_content_ref: None,
            tool_results: vec![],
            output_instructions: make_output_instructions(),
            session_working_memory: vec![],
            conversation_history: vec![],
            persona: None,
            is_onboarding: false,
            is_persona_just_configured: false,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        assert!(
            prompt.contains("Never output secrets"),
            "prompt should include base safety rules"
        );
        assert!(
            prompt.contains("Never attempt to access resources"),
            "prompt should include rule 2"
        );
    }

    #[test]
    fn test_compose_prompt_with_raw_content() {
        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "User wants to reply to an email".to_owned(),
            raw_content_ref: Some(
                "Hi, please review the Q3 budget by Friday. Thanks, Sarah".to_owned(),
            ),
            tool_results: vec![make_step_result(1, "email.read")],
            output_instructions: make_output_instructions(),
            session_working_memory: vec![],
            conversation_history: vec![],
            persona: None,
            is_onboarding: false,
            is_persona_just_configured: false,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        assert!(
            prompt.contains("## Raw Content"),
            "prompt should include raw content section header"
        );
        assert!(
            prompt.contains("please review the Q3 budget by Friday"),
            "prompt should include raw content"
        );
    }

    #[test]
    fn test_compose_prompt_without_raw_content() {
        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "Test".to_owned(),
            raw_content_ref: None,
            tool_results: vec![],
            output_instructions: make_output_instructions(),
            session_working_memory: vec![],
            conversation_history: vec![],
            persona: None,
            is_onboarding: false,
            is_persona_just_configured: false,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        assert!(
            !prompt.contains("## Raw Content"),
            "prompt should NOT include raw content section when None"
        );
    }

    #[test]
    fn test_step_result_serialization() {
        let result = StepResult {
            step: 1,
            tool: "email.list".to_owned(),
            result: serde_json::json!({"emails": [{"id": "msg_1"}]}),
        };

        let json = serde_json::to_string(&result).expect("should serialize");
        let deserialized: StepResult = serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(deserialized.step, 1);
        assert_eq!(deserialized.tool, "email.list");
        assert_eq!(deserialized.result["emails"][0]["id"], "msg_1");
    }

    #[test]
    fn test_compose_prompt_multiple_results() {
        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "User asked to check email and calendar".to_owned(),
            raw_content_ref: None,
            tool_results: vec![
                make_step_result(1, "email.list"),
                StepResult {
                    step: 2,
                    tool: "calendar.freebusy".to_owned(),
                    result: serde_json::json!({"free": true, "date": "2026-03-15"}),
                },
            ],
            output_instructions: make_output_instructions(),
            session_working_memory: vec![],
            conversation_history: vec![],
            persona: None,
            is_onboarding: false,
            is_persona_just_configured: false,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        assert!(
            prompt.contains("email.list"),
            "prompt should include first tool result"
        );
        assert!(
            prompt.contains("calendar.freebusy"),
            "prompt should include second tool result"
        );
    }

    #[test]
    fn test_output_instructions_in_prompt() {
        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "Test".to_owned(),
            raw_content_ref: None,
            tool_results: vec![],
            output_instructions: OutputInstructions {
                sink: "sink:slack:owner".to_owned(),
                max_length: 4000,
                format: "markdown".to_owned(),
            },
            session_working_memory: vec![],
            conversation_history: vec![],
            persona: None,
            is_onboarding: false,
            is_persona_just_configured: false,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        assert!(
            prompt.contains("Format: markdown"),
            "prompt should include the format"
        );
        assert!(
            prompt.contains("Maximum length: 4000"),
            "prompt should include the max length"
        );
    }

    #[test]
    fn test_compose_prompt_includes_conversation_history() {
        use crate::kernel::session::{ConversationTurn, TaskResult};
        use crate::types::SecurityLabel;
        use chrono::Utc;

        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "what did we talk about?".to_owned(),
            raw_content_ref: None,
            tool_results: vec![],
            output_instructions: make_output_instructions(),
            session_working_memory: vec![TaskResult {
                task_id: Uuid::nil(),
                timestamp: Utc::now(),
                request_summary: "distance to the moon".to_owned(),
                tool_outputs: vec![],
                response_summary: "About 384,000 km".to_owned(),
                label: SecurityLabel::Public,
            }],
            conversation_history: vec![
                ConversationTurn {
                    role: "user".to_owned(),
                    summary: "distance to the moon".to_owned(),
                    timestamp: Utc::now(),
                },
                ConversationTurn {
                    role: "assistant".to_owned(),
                    summary: "About 384,000 km".to_owned(),
                    timestamp: Utc::now(),
                },
            ],
            persona: None,
            is_onboarding: false,
            is_persona_just_configured: false,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        assert!(
            prompt.contains("## Session Working Memory"),
            "prompt should include session working memory section"
        );
        assert!(
            prompt.contains("distance to the moon"),
            "prompt should include previous task summary"
        );
        assert!(
            prompt.contains("## Conversation History (background context only"),
            "prompt should include conversation history section with no-summary instruction"
        );
        assert!(
            prompt.contains("About 384,000 km"),
            "prompt should include previous response"
        );
    }

    #[test]
    fn test_compose_prompt_history_format_discourages_summary() {
        use chrono::Utc;

        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "Ok".to_owned(),
            raw_content_ref: None,
            tool_results: vec![],
            output_instructions: make_output_instructions(),
            session_working_memory: vec![],
            conversation_history: vec![
                ConversationTurn {
                    role: "user".to_owned(),
                    summary: "distance to the moon".to_owned(),
                    timestamp: Utc::now(),
                },
                ConversationTurn {
                    role: "assistant".to_owned(),
                    summary: "About 384,000 km".to_owned(),
                    timestamp: Utc::now(),
                },
            ],
            persona: None,
            is_onboarding: false,
            is_persona_just_configured: false,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        // Header includes anti-summary instruction.
        assert!(
            prompt.contains("do NOT summarize or repeat"),
            "history section should include anti-summary instruction"
        );

        // Format is readable lines, not JSON.
        assert!(
            prompt.contains("- user: distance to the moon"),
            "history should use '- role: summary' format"
        );
        assert!(
            prompt.contains("- assistant: About 384,000 km"),
            "history should use '- role: summary' format for assistant"
        );

        // No JSON field names with quotes (old format had `"role":`, `"summary":`).
        assert!(
            !prompt.contains("\"role\""),
            "history should not contain JSON field names"
        );
        assert!(
            !prompt.contains("\"summary\""),
            "history should not contain JSON field names"
        );
    }

    #[test]
    fn test_compose_prompt_no_session_context() {
        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "hello".to_owned(),
            raw_content_ref: None,
            tool_results: vec![],
            output_instructions: make_output_instructions(),
            session_working_memory: vec![],
            conversation_history: vec![],
            persona: None,
            is_onboarding: false,
            is_persona_just_configured: false,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        assert!(
            !prompt.contains("## Session Working Memory"),
            "empty working memory should not produce section header"
        );
        assert!(
            !prompt.contains("## Conversation History"),
            "empty history should not produce section header"
        );
    }

    // ── Persona tests (persona-onboarding spec §2) ──────────────

    #[test]
    fn test_compose_prompt_with_persona() {
        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "Hey".to_owned(),
            raw_content_ref: None,
            tool_results: vec![],
            output_instructions: make_output_instructions(),
            session_working_memory: vec![],
            conversation_history: vec![],
            persona: Some("Atlas. Owner: Igor. Style: concise, dry humor.".to_owned()),
            is_onboarding: false,
            is_persona_just_configured: false,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        assert!(
            prompt.contains("You are Atlas. Owner: Igor. Style: concise, dry humor."),
            "prompt should inject persona identity"
        );
        assert!(
            prompt.contains("Never mention internal system details"),
            "prompt should include anti-leak instruction"
        );
        assert!(
            !prompt.contains("running for the first time"),
            "prompt should NOT include onboarding preamble"
        );
    }

    #[test]
    fn test_compose_prompt_onboarding() {
        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "Hey".to_owned(),
            raw_content_ref: None,
            tool_results: vec![],
            output_instructions: make_output_instructions(),
            session_working_memory: vec![],
            conversation_history: vec![],
            persona: None,
            is_onboarding: true,
            is_persona_just_configured: false,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        assert!(
            prompt.contains("running for the first time"),
            "onboarding prompt should ask user to configure"
        );
        assert!(
            prompt.contains("What should they call you"),
            "onboarding prompt should ask for assistant name"
        );
        assert!(
            !prompt.contains("Never mention internal system details"),
            "onboarding should NOT include anti-leak (no persona yet)"
        );
    }

    #[test]
    fn test_compose_prompt_default_no_persona() {
        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "Hey".to_owned(),
            raw_content_ref: None,
            tool_results: vec![],
            output_instructions: make_output_instructions(),
            session_working_memory: vec![],
            conversation_history: vec![],
            persona: None,
            is_onboarding: false,
            is_persona_just_configured: false,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        // Default: original SYNTHESIZER_ROLE_PROMPT, no persona, no onboarding.
        assert!(
            prompt.contains("You are the Synthesizer"),
            "default prompt should use original role prompt"
        );
        assert!(
            !prompt.contains("running for the first time"),
            "default should NOT have onboarding"
        );
        assert!(
            !prompt.contains("Never mention internal system details"),
            "default should NOT have anti-leak"
        );
    }

    #[test]
    fn test_compose_prompt_persona_just_configured() {
        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "Call yourself Atlas. I'm Igor. Keep it concise.".to_owned(),
            raw_content_ref: None,
            tool_results: vec![],
            output_instructions: make_output_instructions(),
            session_working_memory: vec![],
            conversation_history: vec![],
            persona: Some("Call yourself Atlas. I'm Igor. Keep it concise.".to_owned()),
            is_onboarding: false,
            is_persona_just_configured: true,
            memory_entries: vec![],
            sid: None,
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        assert!(
            prompt.contains("just provided their preferences"),
            "just-configured prompt should include confirmation instruction"
        );
        assert!(
            !prompt.contains("running for the first time"),
            "just-configured should NOT include onboarding"
        );
        assert!(
            !prompt.contains("Never mention internal system details"),
            "just-configured should NOT include anti-leak"
        );
    }

    // ── SID tests (pfar-system-identity-document.md) ──

    #[test]
    fn test_compose_prompt_with_sid_persona_dedup() {
        let sid_text = "You are Atlas. Owner: Igor.\n\nCAPABILITIES:\n- Built-in tools: email\n\nRULES:\n- Never mention internal architecture\n";
        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "Hey".to_owned(),
            raw_content_ref: None,
            tool_results: vec![],
            output_instructions: make_output_instructions(),
            session_working_memory: vec![],
            conversation_history: vec![],
            persona: Some("Atlas. Owner: Igor.".to_owned()),
            is_onboarding: false,
            is_persona_just_configured: false,
            memory_entries: vec![],
            sid: Some(sid_text.to_owned()),
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        // SID should be present.
        assert!(
            prompt.contains("CAPABILITIES:"),
            "prompt should contain SID capabilities"
        );
        // Persona should appear only once (from SID), not duplicated in role section.
        let persona_count = prompt.matches("You are Atlas").count();
        assert_eq!(
            persona_count, 1,
            "persona should appear exactly once (from SID), got {persona_count}"
        );
        // Anti-leak should still be present (from role section).
        assert!(
            prompt.contains("Never mention internal system details"),
            "anti-leak should be in role section when SID is present"
        );
        // Synthesizer role should still be present.
        assert!(
            prompt.contains("You are the Synthesizer"),
            "synthesizer role should still be present"
        );
    }

    #[test]
    fn test_compose_prompt_with_sid_no_persona() {
        // SID present but no persona configured — should use SID for capabilities
        // but not include anti-leak (no persona to protect).
        let sid_text =
            "\nCAPABILITIES:\n- Built-in tools: email\n\nRULES:\n- Never mention internal architecture\n";
        let ctx = SynthesizerContext {
            task_id: Uuid::nil(),
            original_context: "Hey".to_owned(),
            raw_content_ref: None,
            tool_results: vec![],
            output_instructions: make_output_instructions(),
            session_working_memory: vec![],
            conversation_history: vec![],
            persona: None,
            is_onboarding: false,
            is_persona_just_configured: false,
            memory_entries: vec![],
            sid: Some(sid_text.to_owned()),
        };

        let prompt = Synthesizer::compose_prompt(&ctx);

        // SID should be present.
        assert!(
            prompt.contains("CAPABILITIES:"),
            "prompt should contain SID capabilities"
        );
        // Should NOT include anti-leak (no persona to protect).
        assert!(
            !prompt.contains("Never mention internal system details"),
            "no anti-leak when SID present but no persona"
        );
        // Synthesizer role should still be present.
        assert!(
            prompt.contains("You are the Synthesizer"),
            "synthesizer role should still be present"
        );
    }
}
