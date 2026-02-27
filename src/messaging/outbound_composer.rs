//! Restricted-context message composition.
//!
//! Converts agent intent into a natural first-person message using only
//! brief-scoped context. The outbound composer has NO access to USER.md,
//! memories, AGENTS.md, or the main conversation.

use std::sync::Arc;

use rand::Rng;
use tracing::{debug, warn};

use crate::agent::budget::DailyBudget;
use crate::providers::router::ModelRouter;
use crate::providers::{extract_text, CompletionRequest, Message, MessageContent, Role};

use super::brief::TaskBrief;
use super::outbound_context::build_outbound_system_prompt;
use super::outbound_redactor::{OutboundRedactor, RedactionWarning};
use super::MessagingError;

/// Result of outbound composition.
#[derive(Debug, Clone)]
pub struct ComposedMessage {
    /// The composed message text.
    pub text: String,
    /// Privacy warnings detected by the redactor.
    pub warnings: Vec<RedactionWarning>,
    /// Whether the message was blocked by the redactor.
    pub blocked: bool,
}

/// Composes outbound messages using restricted brief-only context.
pub struct OutboundComposer {
    model_router: Arc<ModelRouter>,
    daily_budget: Arc<DailyBudget>,
    redactor: OutboundRedactor,
}

impl OutboundComposer {
    /// Create a new outbound composer.
    pub fn new(
        model_router: Arc<ModelRouter>,
        daily_budget: Arc<DailyBudget>,
        redactor: OutboundRedactor,
    ) -> Self {
        Self {
            model_router,
            daily_budget,
            redactor,
        }
    }

    /// Compose a natural message from agent intent.
    ///
    /// Uses a separate LLM call with restricted context (brief only).
    /// The composed message is scanned by the outbound redactor before
    /// being returned.
    ///
    /// # Errors
    ///
    /// Returns [`MessagingError::CompositionFailed`] on budget exhaustion,
    /// provider resolution failure, or LLM call failure.
    pub async fn compose(
        &self,
        brief: &TaskBrief,
        conversation_history: &[OutboundMessage],
        incoming: Option<&str>,
        agent_intent: &str,
    ) -> Result<ComposedMessage, MessagingError> {
        let system_prompt = build_outbound_system_prompt(brief);

        // Build messages: conversation history + current intent
        let mut messages = Vec::new();

        for msg in conversation_history {
            let role = if msg.is_from_contact {
                Role::User
            } else {
                Role::Assistant
            };
            messages.push(Message {
                role,
                content: MessageContent::Text(msg.text.clone()),
            });
        }

        // Add the current prompt
        let user_prompt = match incoming {
            Some(text) => {
                format!("Contact said: \"{text}\"\nYour intent: {agent_intent}\nWrite your reply.")
            }
            None => format!("Write the opening message.\nYour intent: {agent_intent}"),
        };
        messages.push(Message {
            role: Role::User,
            content: MessageContent::Text(user_prompt),
        });

        // Budget check: estimate tokens from prompt lengths
        let msg_chars: usize = messages.iter().map(|m| m.content.text().len()).sum();
        let estimated_tokens = system_prompt
            .len()
            .saturating_add(msg_chars)
            .saturating_div(4);
        let estimated_u64 = u64::try_from(estimated_tokens).unwrap_or(u64::MAX);
        if let Err(e) = self.daily_budget.check(estimated_u64) {
            return Err(MessagingError::CompositionFailed(format!(
                "budget exhausted: {e}"
            )));
        }

        // Resolve provider for "outbound" role, falling back to default
        let provider = self
            .model_router
            .resolve(Some("outbound"), None)
            .or_else(|_| self.model_router.resolve(None, None))
            .map_err(|e| MessagingError::CompositionFailed(format!("no provider: {e}")))?;

        let request = CompletionRequest {
            messages,
            system: Some(system_prompt),
            tools: vec![],
            max_tokens: Some(1024),
            stop_sequences: vec![],
        };

        let response = provider
            .complete(request)
            .await
            .map_err(|e| MessagingError::CompositionFailed(e.to_string()))?;

        // Record budget usage
        let total_tokens = u64::from(response.usage.input_tokens)
            .saturating_add(u64::from(response.usage.output_tokens));
        self.daily_budget.record(total_tokens);

        let text = extract_text(&response.content);
        debug!(brief_id = %brief.id, text_len = text.len(), "outbound message composed");

        // Scan for privacy violations
        let warnings = self.redactor.scan(&text, brief);
        let blocked = OutboundRedactor::has_blocking_warnings(&warnings);

        if blocked {
            warn!(
                brief_id = %brief.id,
                warning_count = warnings.len(),
                "outbound message blocked by redactor"
            );
        }

        Ok(ComposedMessage {
            text,
            warnings,
            blocked,
        })
    }
}

/// Minimum human-like delay in milliseconds.
const MIN_DELAY_MS: u64 = 2_000;

/// Maximum human-like delay in milliseconds.
const MAX_DELAY_MS: u64 = 15_000;

/// Approximate characters per word for estimating word count.
const CHARS_PER_WORD: usize = 5;

/// Milliseconds of simulated reading time per incoming word.
const READ_MS_PER_WORD: u64 = 50;

/// Maximum additional reading time in milliseconds.
const MAX_READ_TIME_MS: u64 = 5_000;

/// Milliseconds of simulated typing time per outgoing word.
const TYPE_MS_PER_WORD: u64 = 100;

/// Maximum additional typing time in milliseconds.
const MAX_TYPE_TIME_MS: u64 = 10_000;

/// Calculate a human-like delay before sending a WhatsApp reply.
///
/// Simulates reading time (based on incoming message length) plus composing
/// time (based on outgoing message length). Returns the delay in milliseconds,
/// clamped to the 2-15 second range.
pub fn human_like_delay_ms(incoming_len: usize, outgoing_len: usize) -> u64 {
    let mut rng = rand::thread_rng();
    // Base: 2-5 seconds for reading
    let read_ms: u64 = rng.gen_range(MIN_DELAY_MS..5_000);
    // Add ~50ms per word of incoming message (reading speed)
    let incoming_words = incoming_len.saturating_div(CHARS_PER_WORD);
    let read_time = u64::try_from(incoming_words)
        .unwrap_or(u64::MAX)
        .saturating_mul(READ_MS_PER_WORD)
        .min(MAX_READ_TIME_MS);
    // Add ~100ms per word of outgoing message (typing speed)
    let outgoing_words = outgoing_len.saturating_div(CHARS_PER_WORD);
    let type_time = u64::try_from(outgoing_words)
        .unwrap_or(u64::MAX)
        .saturating_mul(TYPE_MS_PER_WORD)
        .min(MAX_TYPE_TIME_MS);
    // Total: 2-15 seconds range, clamped
    read_ms
        .saturating_add(read_time)
        .saturating_add(type_time)
        .clamp(MIN_DELAY_MS, MAX_DELAY_MS)
}

/// A message in the outbound conversation thread.
#[derive(Debug, Clone)]
pub struct OutboundMessage {
    /// The message text.
    pub text: String,
    /// Whether this message was sent by the contact (true) or agent (false).
    pub is_from_contact: bool,
}

/// Load conversation history for a brief from the `outbound_log` table.
///
/// Returns messages ordered chronologically. The `direction` column maps to
/// `is_from_contact`: `"inbound"` means the contact sent the message,
/// `"outbound"` means the agent sent it.
///
/// # Errors
///
/// Returns [`MessagingError::Database`] on SQLite failure.
pub async fn load_conversation_history(
    db: &sqlx::SqlitePool,
    brief_id: &str,
) -> Result<Vec<OutboundMessage>, MessagingError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT message_text, direction FROM outbound_log \
         WHERE brief_id = ?1 AND blocked = FALSE \
         ORDER BY created_at ASC",
    )
    .bind(brief_id)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(text, direction)| OutboundMessage {
            text,
            is_from_contact: direction == "inbound",
        })
        .collect())
}
