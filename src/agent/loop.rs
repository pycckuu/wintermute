//! Agent loop: the core reasoning cycle that drives each session.
//!
//! Each user session runs as an independent Tokio task. The session receives
//! [`SessionEvent`]s via an mpsc channel and drives the LLM reasoning loop
//! for each user message.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use url::Url;

use crate::observer::ObserverEvent;

use crate::agent::approval::ApprovalResult;
use crate::agent::budget::{BudgetStatus, SessionBudget};
use crate::agent::context::{
    apply_compaction, assemble_system_prompt, build_compaction_plan, build_compaction_request,
    estimate_messages_tokens, should_compact, trim_messages, trim_messages_to_fraction,
    COMPACTION_KEEP_LAST,
};
use crate::agent::policy::{check_policy, PolicyContext, PolicyDecision};
use crate::agent::TelegramOutbound;
use crate::config::{AgentConfig, Config};
use crate::memory::{ConversationEntry, Memory, MemoryEngine, MemoryStatus, TrustSource};
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
    /// Optional channel for observer idle events.
    pub observer_tx: Option<mpsc::Sender<ObserverEvent>>,
    /// Loaded System Identity Document content (from IDENTITY.md).
    pub identity_document: Option<String>,
    /// Loaded USER.md content (consolidated long-term memory).
    pub user_md_content: Option<String>,
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

/// Duration of inactivity before triggering observer extraction.
const OBSERVER_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Run a session, processing events until shutdown or channel close.
///
/// This is the top-level entry point spawned as a Tokio task for each user
/// session. It maintains the conversation history and dispatches to the
/// agent reasoning loop on each user message. Includes idle detection for
/// the observer pipeline.
pub async fn run_session(cfg: SessionConfig, mut event_rx: mpsc::Receiver<SessionEvent>) {
    info!(session_id = %cfg.session_id, user_id = cfg.user_id, "session started");

    let mut conversation: Vec<Message> = Vec::new();

    // Bootstrap: fetch recent active memories so the first turn has context
    // about prior interactions. Prevents "cognitive cold start" where the
    // agent has no awareness of what it previously learned.
    // Bounded by a timeout so a contended database cannot delay session start.
    let mut bootstrap_memories = match tokio::time::timeout(
        Duration::from_secs(2),
        cfg.memory.search_by_status(MemoryStatus::Active, 5),
    )
    .await
    {
        Ok(Ok(mems)) => mems,
        Ok(Err(e)) => {
            warn!(error = %e, "bootstrap memory fetch failed, proceeding without");
            Vec::new()
        }
        Err(_) => {
            warn!("bootstrap memory fetch timed out, proceeding without");
            Vec::new()
        }
    };
    if !bootstrap_memories.is_empty() {
        debug!(
            count = bootstrap_memories.len(),
            "loaded bootstrap memories for session"
        );
    }

    let mut last_turn_had_activity = false;
    // Track which warning threshold was last shown to avoid repeating.
    let mut last_warned_percent: u8 = 0;
    // Compaction fires at most once per session to avoid repeated LLM calls.
    let mut compacted_this_session = false;

    loop {
        let event = if last_turn_had_activity {
            // After activity, wait with timeout for observer trigger.
            match tokio::time::timeout(OBSERVER_IDLE_TIMEOUT, event_rx.recv()).await {
                Ok(Some(event)) => event,
                Ok(None) => break, // channel closed
                Err(_) => {
                    // Idle timeout — notify observer if configured.
                    // Only send the last MAX_OBSERVER_TAIL messages to avoid
                    // cloning the entire conversation (observer truncates anyway).
                    if let Some(ref observer_tx) = cfg.observer_tx {
                        let start = conversation.len().saturating_sub(20);
                        let event = ObserverEvent {
                            session_id: cfg.session_id.clone(),
                            user_id: cfg.user_id,
                            messages: conversation[start..].to_vec(),
                        };
                        if let Err(e) = observer_tx.try_send(event) {
                            debug!(error = %e, "failed to send observer event (non-blocking)");
                        }
                    }
                    last_turn_had_activity = false;
                    continue;
                }
            }
        } else {
            // No recent activity — wait indefinitely.
            match event_rx.recv().await {
                Some(event) => event,
                None => break,
            }
        };

        match event {
            SessionEvent::UserMessage(text) => {
                last_turn_had_activity = true;
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

                // Renew paused session on new user message
                if cfg.budget.is_paused() {
                    if cfg.budget.renew() {
                        cfg.budget.set_paused(false);
                        last_warned_percent = 0;
                        info!(session_id = %cfg.session_id, "budget renewed by user message");
                    } else {
                        send_text(
                            &cfg,
                            "Daily budget still exhausted. I'll be back tomorrow, \
                             or adjust the limit in config.toml under [budget].max_tokens_per_day.",
                        )
                        .await;
                        continue;
                    }
                }

                // Run the agent reasoning turn
                run_agent_turn(
                    &cfg,
                    &mut conversation,
                    &mut last_warned_percent,
                    &mut compacted_this_session,
                    &mut bootstrap_memories,
                )
                .await;
            }
            SessionEvent::ApprovalResolved(result) => {
                debug!(session_id = %cfg.session_id, "received approval resolution");
                handle_approval_resolved(
                    &cfg,
                    &mut conversation,
                    result,
                    &mut last_warned_percent,
                    &mut compacted_this_session,
                    &mut bootstrap_memories,
                )
                .await;
            }
            SessionEvent::Shutdown => {
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

/// Maximum retry attempts on context overflow before giving up.
const MAX_OVERFLOW_RETRIES: u32 = 3;

/// Fraction of context to keep when retrying after overflow (aggressive trim).
const OVERFLOW_TRIM_FRACTION: f64 = 0.5;

/// Execute one full agent reasoning turn (may involve multiple LLM calls).
///
/// The inner loop continues as long as the LLM returns `StopReason::ToolUse`,
/// executing each tool call and feeding results back. `bootstrap_memories`
/// are merged into the first turn's context and drained so that subsequent
/// turns rely solely on query-driven memory search.
async fn run_agent_turn(
    cfg: &SessionConfig,
    conversation: &mut Vec<Message>,
    last_warned_percent: &mut u8,
    compacted_this_session: &mut bool,
    bootstrap_memories: &mut Vec<Memory>,
) {
    let mut tool_call_count: u32 = 0;

    // Context compaction: compress older messages if budget usage is high.
    // Only fires once per session to avoid repeated LLM summarization calls.
    if !*compacted_this_session && should_compact(cfg.budget.session_percent()) {
        if let Some(plan) = build_compaction_plan(conversation, COMPACTION_KEEP_LAST) {
            info!(
                messages_to_compact = plan.messages_to_compact.len(),
                estimated_savings = plan.estimated_savings_tokens,
                "attempting context compaction"
            );

            // Target tokens for the summary: ~20% of estimated savings
            #[allow(clippy::arithmetic_side_effects)]
            let target_tokens = plan.estimated_savings_tokens / 5;
            let compaction_messages = build_compaction_request(&plan, target_tokens.max(200));

            // Budget check before compaction LLM call
            let compaction_estimate = estimate_messages_tokens(&compaction_messages);
            if let Err(e) = cfg.budget.check_budget(compaction_estimate) {
                warn!(error = %e, "skipping compaction: budget insufficient");
            } else if let Ok(provider) = cfg.router.resolve(None, None) {
                let request = CompletionRequest {
                    messages: compaction_messages,
                    system: Some(
                        "You are a conversation summarizer. Produce a concise summary.".to_owned(),
                    ),
                    tools: vec![],
                    max_tokens: Some(2048),
                    stop_sequences: vec![],
                };

                match provider.complete(request).await {
                    Ok(response) => {
                        let summary = extract_assistant_text(&response.content);
                        if !summary.is_empty() {
                            cfg.budget.record_usage(
                                u64::from(response.usage.input_tokens),
                                u64::from(response.usage.output_tokens),
                            );
                            // Compaction output is model-authored text and therefore untrusted.
                            // Redact before inserting it into long-lived conversation state.
                            let redacted_summary = cfg.tool_router.redactor().redact(&summary);
                            *conversation =
                                apply_compaction(&redacted_summary, plan.messages_to_keep);
                            *compacted_this_session = true;
                            info!("context compaction applied");
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "context compaction LLM call failed, continuing without");
                    }
                }
            }
        }
    }

    loop {
        // Step 1: Search for relevant memories
        let last_query = last_user_text(conversation);
        let mut memories = match cfg.memory.search(&last_query, 5).await {
            Ok(mems) => mems,
            Err(e) => {
                warn!(error = %e, "memory search failed, proceeding without memories");
                Vec::new()
            }
        };

        // Merge bootstrap memories (consumed on first turn only).
        merge_bootstrap_memories(&mut memories, bootstrap_memories);

        // Step 2: Assemble system prompt
        let pending_approvals = cfg.approval_manager.pending_count(&cfg.session_id);
        let current_time = chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string();

        let tools = cfg.tool_router.tool_definitions(
            cfg.config.budget.max_dynamic_tools_per_turn,
            Some(&last_query),
        );
        let core_tool_count = crate::tools::core::core_tool_definitions().len();

        let system_prompt = assemble_system_prompt(
            &cfg.agent_config.personality.soul,
            cfg.identity_document.as_deref(),
            cfg.user_md_content.as_deref(),
            cfg.policy_context.executor_kind,
            tools.len().saturating_sub(core_tool_count),
            &memories,
            pending_approvals,
            &current_time,
        );

        // Step 3: Resolve provider
        let provider = match cfg.router.resolve(None, None) {
            Ok(p) => p,
            Err(e) => {
                error!(error = %e, "failed to resolve LLM provider");
                send_text(cfg, &format!("Provider error: {e}")).await;
                break;
            }
        };

        // Step 4–5: Trim, budget check, LLM call — with overflow retry
        let mut trimmed = trim_messages(conversation, cfg.config.budget.max_tokens_per_session);
        let mut overflow_retries: u32 = 0;

        let response = loop {
            let estimated = estimate_messages_tokens(&trimmed);
            if let Err(e) = cfg.budget.check_budget(estimated) {
                pause_session(cfg, e.scope().label()).await;
                return;
            }

            // Inject budget warning as system message when crossing a threshold
            let (status, scope) = cfg.budget.budget_status();
            match &status {
                BudgetStatus::Warning { percent, .. } if *percent > *last_warned_percent => {
                    let scope_label = scope.label();
                    let remaining = scope.remaining(&cfg.budget);
                    let note = format!(
                        "[System: {scope_label} budget at {percent}%. \
                         ~{remaining} tokens remaining. Consider wrapping up.]"
                    );
                    trimmed.push(Message {
                        role: Role::User,
                        content: MessageContent::Text(note),
                    });
                    *last_warned_percent = *percent;
                    info!(percent, scope = scope_label, "budget warning injected");
                }
                BudgetStatus::Exhausted => {
                    pause_session(cfg, scope.label()).await;
                    return;
                }
                _ => {}
            }

            let request = CompletionRequest {
                messages: trimmed.clone(),
                system: Some(system_prompt.clone()),
                tools: tools.clone(),
                max_tokens: Some(DEFAULT_MAX_RESPONSE_TOKENS),
                stop_sequences: vec![],
            };

            match provider.complete(request).await {
                Ok(r) => break r,
                Err(e) if e.is_context_overflow() && overflow_retries < MAX_OVERFLOW_RETRIES => {
                    overflow_retries = overflow_retries.saturating_add(1);
                    let fraction =
                        OVERFLOW_TRIM_FRACTION.powi(i32::try_from(overflow_retries).unwrap_or(3));
                    warn!(
                        retry = overflow_retries,
                        fraction, "context overflow, trimming more aggressively"
                    );
                    trimmed = trim_messages_to_fraction(
                        conversation,
                        cfg.config.budget.max_tokens_per_session,
                        fraction,
                    );
                }
                Err(e) => {
                    error!(error = %e, "LLM completion failed");
                    send_text(cfg, &format!("LLM error: {e}")).await;
                    return;
                }
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
                    let trusted_domain = trusted_domain_for_tool(&cfg.memory, name, input).await;
                    let decision = check_policy(name, input, &cfg.policy_context, &|domain| {
                        trusted_domain.as_deref() == Some(domain)
                    });

                    let result = match decision {
                        PolicyDecision::Allow => {
                            cfg.tool_router
                                .execute_for_user(name, input, Some(cfg.user_id))
                                .await
                        }
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
        let assistant_text = extract_assistant_text(&response.content);

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
/// feeding the result back into the conversation, then triggering another
/// agent turn.
async fn handle_approval_resolved(
    cfg: &SessionConfig,
    conversation: &mut Vec<Message>,
    result: ApprovalResult,
    last_warned_percent: &mut u8,
    compacted_this_session: &mut bool,
    bootstrap_memories: &mut Vec<Memory>,
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

            if let Some(domain) = tool_domain(&tool_name, &input) {
                if let Err(e) = cfg.memory.trust_domain(&domain, TrustSource::User).await {
                    warn!(error = %e, domain, "failed to persist approved trusted domain");
                }
            }

            let tool_result = cfg
                .tool_router
                .execute_for_user(&tool_name, &input, Some(cfg.user_id))
                .await;
            send_text(cfg, &format!("Approved tool <b>{tool_name}</b> executed.")).await;

            // Add the tool result to conversation and trigger another turn
            conversation.push(Message {
                role: Role::User,
                content: MessageContent::Text(format!(
                    "Tool {tool_name} was approved and returned: {}",
                    tool_result.content
                )),
            });

            run_agent_turn(
                cfg,
                conversation,
                last_warned_percent,
                compacted_this_session,
                bootstrap_memories,
            )
            .await;
        }
        ApprovalResult::Denied { tool_name, .. } => {
            send_text(cfg, &format!("Tool <b>{tool_name}</b> was denied by user.")).await;

            conversation.push(Message {
                role: Role::User,
                content: MessageContent::Text(format!(
                    "User denied the tool call for {tool_name}."
                )),
            });

            run_agent_turn(
                cfg,
                conversation,
                last_warned_percent,
                compacted_this_session,
                bootstrap_memories,
            )
            .await;
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

/// Maximum combined memories (query results + bootstrap) to feed into context.
const MAX_CONTEXT_MEMORIES: usize = 10;

/// Merge bootstrap memories into the query results, deduplicating by id.
///
/// Bootstrap memories are loaded once at session start to prevent "cognitive
/// cold start". They are drained on first call so subsequent turns use only
/// query-driven results.
fn merge_bootstrap_memories(memories: &mut Vec<Memory>, bootstrap: &mut Vec<Memory>) {
    if bootstrap.is_empty() {
        return;
    }

    // Both query results and bootstrap memories come from the database and
    // always have Some(id). Deduplicate on the id value; entries without an
    // id are always included to avoid silently dropping unpersisted entries.
    let existing_ids: std::collections::HashSet<i64> =
        memories.iter().filter_map(|m| m.id).collect();

    for mem in bootstrap.drain(..) {
        if mem.id.is_none_or(|id| !existing_ids.contains(&id)) {
            memories.push(mem);
        }
    }

    // Cap to avoid oversized system prompts.
    memories.truncate(MAX_CONTEXT_MEMORIES);
}

/// Extract plain text from assistant response content parts.
fn extract_assistant_text(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Pause the session on budget exhaustion and notify the user.
async fn pause_session(cfg: &SessionConfig, scope_label: &str) {
    cfg.budget.set_paused(true);
    let msg = format!(
        "{scope_label} token budget reached. Send another message to continue \
         with a fresh allocation, or /reset to start a new conversation. \
         Adjust limits in config.toml under [budget]."
    );
    send_text(cfg, &msg).await;
}

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

/// Resolve trusted domain from trust ledger for domain-sensitive tools.
async fn trusted_domain_for_tool(
    memory: &MemoryEngine,
    tool_name: &str,
    input: &serde_json::Value,
) -> Option<String> {
    let domain = tool_domain(tool_name, input)?;
    match memory.is_domain_trusted(&domain).await {
        Ok(true) => Some(domain),
        Ok(false) => None,
        Err(e) => {
            warn!(error = %e, "failed to read trust ledger");
            None
        }
    }
}

/// Extract URL domain for tools that require domain policy evaluation.
fn tool_domain(tool_name: &str, input: &serde_json::Value) -> Option<String> {
    let domain_sensitive = match tool_name {
        "web_request" => true,
        "browser" => input.get("action").and_then(|v| v.as_str()) == Some("navigate"),
        _ => false,
    };
    if !domain_sensitive {
        return None;
    }
    let url_str = input.get("url").and_then(|v| v.as_str())?;
    let parsed = Url::parse(url_str).ok()?;
    let host = parsed.host_str()?.to_owned();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}
