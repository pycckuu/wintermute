//! Observer pipeline: background learning from conversations.
//!
//! Receives conversation snapshots from idle sessions, extracts facts and
//! procedures via LLM, and stages them as pending memories for promotion.
//!
//! The observer runs as an independent Tokio task. Sessions signal idle state
//! by sending [`ObserverEvent`]s through an mpsc channel. The observer uses
//! a cheap/local model (resolved via the "observer" role) to minimize cost.

pub mod extractor;
pub mod staging;

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::agent::budget::DailyBudget;
use crate::agent::TelegramOutbound;
use crate::config::{LearningConfig, PromotionMode};
use crate::executor::redactor::Redactor;
use crate::memory::MemoryEngine;
use crate::providers::router::ModelRouter;
use crate::providers::Message;

/// An event sent from a session loop when it goes idle.
#[derive(Debug, Clone)]
pub struct ObserverEvent {
    /// Session that went idle.
    pub session_id: String,
    /// User who owns the session.
    pub user_id: i64,
    /// Snapshot of recent conversation messages.
    pub messages: Vec<Message>,
}

/// Shared dependencies for the observer pipeline.
pub struct ObserverDeps {
    /// Memory engine for persistence.
    pub memory: Arc<MemoryEngine>,
    /// Model router for provider resolution.
    pub router: Arc<ModelRouter>,
    /// Shared daily budget (observer calls count toward daily limit).
    pub daily_budget: Arc<DailyBudget>,
    /// Redactor for sanitizing LLM output.
    pub redactor: Redactor,
    /// Learning configuration (promotion mode, threshold).
    pub learning_config: LearningConfig,
    /// Channel for outbound Telegram messages.
    pub telegram_tx: mpsc::Sender<TelegramOutbound>,
}

/// Maximum conversation messages to send to the observer model.
const MAX_OBSERVER_MESSAGES: usize = 20;

/// Run the observer background task.
///
/// Processes [`ObserverEvent`]s from session loops. For each idle session,
/// extracts facts and procedures via LLM and stages them as pending memories.
/// Exits when the channel closes.
pub async fn run_observer(deps: ObserverDeps, mut event_rx: mpsc::Receiver<ObserverEvent>) {
    info!("observer pipeline started");

    while let Some(event) = event_rx.recv().await {
        if deps.learning_config.promotion_mode == PromotionMode::Off {
            debug!(
                session_id = %event.session_id,
                "observer skipping extraction (promotion_mode = off)"
            );
            continue;
        }

        // Truncate conversation to avoid sending huge context to cheap model.
        let messages: Vec<Message> = if event.messages.len() > MAX_OBSERVER_MESSAGES {
            let start = event.messages.len().saturating_sub(MAX_OBSERVER_MESSAGES);
            event.messages[start..].to_vec()
        } else {
            event.messages
        };

        if messages.is_empty() {
            debug!(session_id = %event.session_id, "observer skipping empty conversation");
            continue;
        }

        // Extract facts and procedures from the conversation.
        let extractions =
            match extractor::extract(&messages, &deps.router, &deps.redactor, &deps.daily_budget)
                .await
            {
                Ok(e) => e,
                Err(e) => {
                    warn!(
                        error = %e,
                        session_id = %event.session_id,
                        "observer extraction failed"
                    );
                    continue;
                }
            };

        if extractions.is_empty() {
            debug!(session_id = %event.session_id, "observer found no extractions");
            continue;
        }

        info!(
            session_id = %event.session_id,
            count = extractions.len(),
            "observer extracted memories"
        );

        // Stage extractions as pending memories.
        match staging::stage_extractions(&extractions, &deps.memory, &event.session_id).await {
            Ok(result) => {
                info!(
                    staged = result.staged,
                    duplicates = result.duplicates,
                    contradictions = result.contradictions,
                    "observer staging complete"
                );
            }
            Err(e) => {
                error!(error = %e, "observer staging failed");
                continue;
            }
        }

        // Run promotion check if in auto mode.
        if deps.learning_config.promotion_mode == PromotionMode::Auto {
            match staging::check_promotions(
                &deps.memory,
                &deps.learning_config,
                &deps.telegram_tx,
                event.user_id,
            )
            .await
            {
                Ok(result) => {
                    if result.promoted > 0 {
                        info!(
                            promoted = result.promoted,
                            "observer auto-promoted memories"
                        );
                    }
                }
                Err(e) => {
                    warn!(error = %e, "observer promotion check failed");
                }
            }
        }
    }

    info!("observer pipeline shut down (channel closed)");
}
