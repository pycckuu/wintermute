//! Agent loop: the core reasoning cycle that drives each session.
//!
//! Each user session runs as an independent Tokio task. The session receives
//! [`SessionEvent`]s via an mpsc channel and drives the LLM reasoning loop
//! for each user message.

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::agent::approval::ApprovalResult;
use crate::agent::budget::SessionBudget;
use crate::agent::context::{assemble_system_prompt, estimate_messages_tokens, trim_messages};
use crate::agent::policy::{check_policy, PolicyContext, PolicyDecision};
use crate::agent::TelegramOutbound;
use crate::config::{AgentConfig, Config};
use crate::memory::{ConversationEntry, MemoryEngine};
use crate::providers::router::ModelRouter;
use crate::providers::{CompletionRequest, ContentPart, Message, MessageContent, Role, StopReason};
use crate::tools::ToolRouter;

use super::approval::ApprovalManager;

// ---------------------------------------------------------------------------
// Session events
// ---------------------------------------------------------------------------

/// Events that can be delivered to a running session.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// A new user message to process.
    UserMessage(String),
    /// An approval callback has been resolved.
    ApprovalResolved(ApprovalResult),
    /// Graceful shutdown signal.
    Shutdown,
}

// ---------------------------------------------------------------------------
// Session configuration
// ---------------------------------------------------------------------------

/// All shared resources needed by a session.
///
/// Bundled into a single struct to avoid long parameter lists.
pub struct SessionConfig {
    /// Unique session identifier.
    pub session_id: String,
    /// Telegram user ID that owns this session.
    pub user_id: i64,
    /// Model router for provider resolution.
    pub router: Arc<ModelRouter>,
    /// Tool router for tool execution.
    pub tool_router: Arc<ToolRouter>,
    /// Memory engine for persistence.
    pub memory: Arc<MemoryEngine>,
    /// Per-session budget tracker.
    pub budget: SessionBudget,
    /// Approval manager for tool confirmations.
    pub approval_manager: Arc<ApprovalManager>,
    /// Policy evaluation context.
    pub policy_context: PolicyContext,
    /// Channel for outbound Telegram messages.
    pub telegram_tx: mpsc::Sender<TelegramOutbound>,
    /// Human-owned configuration.
    pub config: Arc<Config>,
    /// Agent-owned configuration.
    pub agent_config: Arc<AgentConfig>,
}

impl std::fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionConfig")
            .field("session_id", &self.session_id)
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Main session loop
// ---------------------------------------------------------------------------

/// Run a session, processing events until shutdown or channel close.
///
/// This is the top-level entry point spawned as a Tokio task for each user
/// session. It maintains the conversation history and dispatches to the
/// agent reasoning loop on each user message.
pub async fn run_session(cfg: SessionConfig, mut event_rx: mpsc::Receiver<SessionEvent>) {
    info!(session_id = %cfg.session_id, user_id = cfg.user_id, "session started");

    let mut conversation: Vec<Message> = Vec::new();

    loop {
        match event_rx.recv().await {
            Some(SessionEvent::UserMessage(text)) => {
                debug!(session_id = %cfg.session_id, "received user message");

                // Add user message to conversation
                conversation.push(Message {
                    role: Role::User,
                    content: MessageContent::Text(text.clone()),
                });

                // Persist the user message
                let entry = ConversationEntry {
                    session_id: cfg.session_id.clone(),
                    role: "user".to_owned(),
                    content: text,
                    tokens_used: None,
                };
                if let Err(e) = cfg.memory.save_conversation(entry).await {
                    warn!(error = %e, "failed to save user conversation entry");
                }

                // Run the agent reasoning turn
                run_agent_turn(&cfg, &mut conversation).await;
            }
            Some(SessionEvent::ApprovalResolved(result)) => {
                debug!(session_id = %cfg.session_id, "received approval resolution");
                handle_approval_resolved(&cfg, &mut conversation, result).await;
            }
            Some(SessionEvent::Shutdown) | None => {
                info!(session_id = %cfg.session_id, "session shutting down");
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Agent reasoning turn
// ---------------------------------------------------------------------------

/// Maximum tokens to request from the LLM per call.
const DEFAULT_MAX_RESPONSE_TOKENS: u32 = 4096;

/// Execute one full agent reasoning turn (may involve multiple LLM calls).
///
/// The inner loop continues as long as the LLM returns `StopReason::ToolUse`,
/// executing each tool call and feeding results back.
async fn run_agent_turn(cfg: &SessionConfig, conversation: &mut Vec<Message>) {
    let mut tool_call_count: u32 = 0;

    loop {
        // Step 1: Search for relevant memories
        let memories = match cfg.memory.search(&last_user_text(conversation), 5).await {
            Ok(mems) => mems,
            Err(e) => {
                warn!(error = %e, "memory search failed, proceeding without memories");
                Vec::new()
            }
        };

        // Step 2: Assemble system prompt
        let pending_approvals = cfg.approval_manager.pending_count(&cfg.session_id);
        let current_time = chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string();

        let tools = cfg
            .tool_router
            .tool_definitions(cfg.config.budget.max_dynamic_tools_per_turn);

        let system_prompt = assemble_system_prompt(
            &cfg.agent_config.personality.soul,
            cfg.policy_context.executor_kind,
            tools.len().saturating_sub(7), // subtract core tool count
            &memories,
            pending_approvals,
            &current_time,
        );

        // Step 3: Trim conversation for context window
        let trimmed = trim_messages(conversation, cfg.config.budget.max_tokens_per_session);

        // Step 4: Budget check before LLM call
        let estimated = estimate_messages_tokens(&trimmed);
        if let Err(e) = cfg.budget.check_budget(estimated) {
            send_text(cfg, &format!("Budget exceeded: {e}")).await;
            break;
        }

        // Step 5: Resolve provider and make LLM call
        let provider = match cfg.router.resolve(None, None) {
            Ok(p) => p,
            Err(e) => {
                error!(error = %e, "failed to resolve LLM provider");
                send_text(cfg, &format!("Provider error: {e}")).await;
                break;
            }
        };

        let request = CompletionRequest {
            messages: trimmed,
            system: Some(system_prompt),
            tools,
            max_tokens: Some(DEFAULT_MAX_RESPONSE_TOKENS),
            stop_sequences: vec![],
        };

        let response = match provider.complete(request).await {
            Ok(r) => r,
            Err(e) => {
                error!(error = %e, "LLM completion failed");
                send_text(cfg, &format!("LLM error: {e}")).await;
                break;
            }
        };

        // Step 6: Record token usage
        cfg.budget.record_usage(
            u64::from(response.usage.input_tokens),
            u64::from(response.usage.output_tokens),
        );

        // Step 7: Process response content parts
        let mut tool_results: Vec<(String, crate::tools::ToolResult)> = Vec::new();
        let mut assistant_content: Vec<ContentPart> = Vec::new();

        for part in &response.content {
            match part {
                ContentPart::Text { text } => {
                    assistant_content.push(part.clone());
                    send_text(cfg, text).await;
                }
                ContentPart::ToolUse { id, name, input } => {
                    assistant_content.push(part.clone());

                    // Check per-turn tool call limit
                    tool_call_count = tool_call_count.saturating_add(1);
                    if let Err(e) = cfg.budget.check_tool_calls(tool_call_count) {
                        tool_results.push((
                            id.clone(),
                            crate::tools::ToolResult::error(format!("Tool call limit: {e}")),
                        ));
                        continue;
                    }

                    // Policy gate
                    let decision = check_policy(name, input, &cfg.policy_context, &|domain| {
                        cfg.policy_context
                            .allowed_domains
                            .iter()
                            .any(|d| d == domain)
                    });

                    let result = match decision {
                        PolicyDecision::Allow => cfg.tool_router.execute(name, input).await,
                        PolicyDecision::RequireApproval => {
                            let approval_id = cfg.approval_manager.request(
                                name.clone(),
                                input.clone(),
                                cfg.session_id.clone(),
                                cfg.user_id,
                            );

                            let _ = cfg
                                .telegram_tx
                                .send(TelegramOutbound {
                                    user_id: cfg.user_id,
                                    text: Some(format!("Tool <b>{name}</b> needs approval")),
                                    file_path: None,
                                    approval_keyboard: Some((approval_id, name.clone())),
                                })
                                .await;

                            crate::tools::ToolResult::success(
                                "Waiting for your approval. I'll continue once you respond.",
                            )
                        }
                        PolicyDecision::Deny(reason) => {
                            crate::tools::ToolResult::error(format!("Denied: {reason}"))
                        }
                    };

                    tool_results.push((id.clone(), result));
                }
                ContentPart::ToolResult { .. } => {
                    // Unexpected in response; skip
                }
            }
        }

        // Step 8: Add assistant message to conversation
        conversation.push(Message {
            role: Role::Assistant,
            content: MessageContent::Parts(assistant_content),
        });

        // Step 9: Persist assistant text to conversation log
        let assistant_text: String = response
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        if !assistant_text.is_empty() {
            let total_tokens = response
                .usage
                .input_tokens
                .saturating_add(response.usage.output_tokens);
            let tokens_i32 = i32::try_from(total_tokens).unwrap_or(i32::MAX);
            let entry = ConversationEntry {
                session_id: cfg.session_id.clone(),
                role: "assistant".to_owned(),
                content: assistant_text,
                tokens_used: Some(tokens_i32),
            };
            if let Err(e) = cfg.memory.save_conversation(entry).await {
                warn!(error = %e, "failed to save assistant conversation entry");
            }
        }

        // Step 10: If there were tool calls, add tool results to conversation
        if !tool_results.is_empty() {
            let result_parts: Vec<ContentPart> = tool_results
                .iter()
                .map(|(id, result)| ContentPart::ToolResult {
                    tool_use_id: id.clone(),
                    content: result.content.clone(),
                    is_error: result.is_error,
                })
                .collect();

            conversation.push(Message {
                role: Role::User,
                content: MessageContent::Parts(result_parts),
            });
        }

        // Step 11: If stop reason is not ToolUse, we're done
        if response.stop_reason != StopReason::ToolUse {
            break;
        }
        // Otherwise loop back for another LLM call
    }
}

// ---------------------------------------------------------------------------
// Approval handling
// ---------------------------------------------------------------------------

/// Handle a resolved approval by executing the tool (if approved) and
/// feeding the result back into the conversation.
async fn handle_approval_resolved(
    cfg: &SessionConfig,
    conversation: &mut Vec<Message>,
    result: ApprovalResult,
) {
    match result {
        ApprovalResult::Approved {
            tool_name,
            tool_input,
            ..
        } => {
            let input: serde_json::Value = match serde_json::from_str(&tool_input) {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "failed to parse approved tool input, using null");
                    serde_json::Value::Null
                }
            };

            let tool_result = cfg.tool_router.execute(&tool_name, &input).await;
            send_text(cfg, &format!("Approved tool <b>{tool_name}</b> executed.")).await;

            // Add the tool result to conversation and trigger another turn
            conversation.push(Message {
                role: Role::User,
                content: MessageContent::Text(format!(
                    "Tool {tool_name} was approved and returned: {}",
                    tool_result.content
                )),
            });

            run_agent_turn(cfg, conversation).await;
        }
        ApprovalResult::Denied { tool_name, .. } => {
            send_text(cfg, &format!("Tool <b>{tool_name}</b> was denied by user.")).await;

            conversation.push(Message {
                role: Role::User,
                content: MessageContent::Text(format!(
                    "User denied the tool call for {tool_name}."
                )),
            });

            run_agent_turn(cfg, conversation).await;
        }
        ApprovalResult::Expired => {
            send_text(cfg, "An approval request has expired.").await;
        }
        ApprovalResult::NotFound => {
            warn!(session_id = %cfg.session_id, "approval not found during resolution");
        }
        ApprovalResult::WrongUser => {
            warn!(session_id = %cfg.session_id, "wrong user attempted approval resolution");
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Send a text message to the user via the Telegram outbound channel.
async fn send_text(cfg: &SessionConfig, text: &str) {
    let msg = TelegramOutbound {
        user_id: cfg.user_id,
        text: Some(text.to_owned()),
        file_path: None,
        approval_keyboard: None,
    };
    if let Err(e) = cfg.telegram_tx.send(msg).await {
        error!(error = %e, "failed to send outbound telegram message");
    }
}

/// Extract the last user text from the conversation for memory search.
fn last_user_text(conversation: &[Message]) -> String {
    conversation
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| m.content.text())
        .unwrap_or_default()
}
