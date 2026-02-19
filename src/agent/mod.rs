//! Agent session management: budget tracking, policy gates, approval flows,
//! context assembly, reasoning loop, and session routing.
//!
//! The [`SessionRouter`] manages per-user sessions as independent Tokio tasks,
//! each driven by `loop::run_session`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

pub mod approval;
pub mod budget;
pub mod context;
pub mod r#loop;
pub mod policy;

pub use r#loop::SessionEvent;

use crate::config::{AgentConfig, Config};
use crate::memory::MemoryEngine;
use crate::providers::router::ModelRouter;
use crate::tools::ToolRouter;

use self::approval::{ApprovalManager, ApprovalResult};
use self::budget::{DailyBudget, SessionBudget};
use self::policy::PolicyContext;
use self::r#loop::SessionConfig;

/// Outbound message from agent to Telegram.
#[derive(Debug, Clone)]
pub struct TelegramOutbound {
    /// Target Telegram user ID.
    pub user_id: i64,
    /// Optional text content (HTML formatted).
    pub text: Option<String>,
    /// Optional file path to send as attachment.
    pub file_path: Option<String>,
    /// Optional approval keyboard (approval_id, description).
    pub approval_keyboard: Option<(String, String)>,
}

/// Session channel buffer size.
const SESSION_CHANNEL_CAPACITY: usize = 64;

// ---------------------------------------------------------------------------
// Session router
// ---------------------------------------------------------------------------

/// Routes messages to per-user sessions, creating them on demand.
///
/// Each session runs as an independent Tokio task consuming [`SessionEvent`]s
/// from a bounded mpsc channel. Dead sessions are cleaned up on next message.
pub struct SessionRouter {
    /// Active session senders keyed by session ID (e.g. "user_12345").
    sessions: Mutex<HashMap<String, mpsc::Sender<SessionEvent>>>,
    /// Model router for provider resolution.
    router: Arc<ModelRouter>,
    /// Tool router for tool dispatch.
    tool_router: Arc<ToolRouter>,
    /// Memory engine for persistence.
    memory: Arc<MemoryEngine>,
    /// Shared daily budget across all sessions.
    daily_budget: Arc<DailyBudget>,
    /// Approval manager for tool confirmations.
    approval_manager: Arc<ApprovalManager>,
    /// Policy evaluation context.
    policy_context: PolicyContext,
    /// Channel for outbound Telegram messages.
    telegram_tx: mpsc::Sender<TelegramOutbound>,
    /// Human-owned configuration.
    config: Arc<Config>,
    /// Agent-owned configuration.
    agent_config: Arc<AgentConfig>,
}

impl std::fmt::Debug for SessionRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRouter")
            .field("config", &"...")
            .finish_non_exhaustive()
    }
}

impl SessionRouter {
    /// Create a new session router with all shared dependencies.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        router: Arc<ModelRouter>,
        tool_router: Arc<ToolRouter>,
        memory: Arc<MemoryEngine>,
        daily_budget: Arc<DailyBudget>,
        approval_manager: Arc<ApprovalManager>,
        policy_context: PolicyContext,
        telegram_tx: mpsc::Sender<TelegramOutbound>,
        config: Arc<Config>,
        agent_config: Arc<AgentConfig>,
    ) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            router,
            tool_router,
            memory,
            daily_budget,
            approval_manager,
            policy_context,
            telegram_tx,
            config,
            agent_config,
        }
    }

    /// Route a user message to the appropriate session, creating one if needed.
    ///
    /// Session key format: `user_{user_id}`.
    ///
    /// If the channel for an existing session is full or closed, the dead session
    /// is replaced with a fresh one.
    ///
    /// # Errors
    ///
    /// Returns an error if the event cannot be sent after creating a new session.
    pub async fn route_message(&self, user_id: i64, text: String) -> anyhow::Result<()> {
        let session_key = format!("user_{user_id}");

        let mut sessions = self.sessions.lock().await;

        // Try to send to existing session
        if let Some(tx) = sessions.get(&session_key) {
            match tx.try_send(SessionEvent::UserMessage(text.clone())) {
                Ok(()) => return Ok(()),
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    info!(session = %session_key, "session channel closed, creating new session");
                    sessions.remove(&session_key);
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!(session = %session_key, "session channel full, replacing session");
                    sessions.remove(&session_key);
                }
            }
        }

        // Create a new session
        let (tx, rx) = mpsc::channel(SESSION_CHANNEL_CAPACITY);
        let session_cfg = self.build_session_config(session_key.clone(), user_id);

        tokio::spawn(r#loop::run_session(session_cfg, rx));

        tx.send(SessionEvent::UserMessage(text))
            .await
            .map_err(|e| anyhow::anyhow!("failed to send initial message to new session: {e}"))?;

        sessions.insert(session_key, tx);
        Ok(())
    }

    /// Route a resolved approval to the session that requested it.
    ///
    /// # Errors
    ///
    /// Returns an error if the session is not found or the channel is closed.
    pub async fn route_approval(&self, result: ApprovalResult) -> anyhow::Result<()> {
        let session_id = match &result {
            ApprovalResult::Approved { session_id, .. } => session_id.clone(),
            ApprovalResult::Denied { session_id, .. } => session_id.clone(),
            ApprovalResult::Expired | ApprovalResult::NotFound | ApprovalResult::WrongUser => {
                return Ok(());
            }
        };

        let sessions = self.sessions.lock().await;
        if let Some(tx) = sessions.get(&session_id) {
            tx.send(SessionEvent::ApprovalResolved(result))
                .await
                .map_err(|e| {
                    anyhow::anyhow!("failed to send approval to session {session_id}: {e}")
                })?;
        } else {
            warn!(session_id = %session_id, "no active session for approval resolution");
        }

        Ok(())
    }

    /// Shut down all active sessions gracefully.
    pub async fn shutdown_all(&self) {
        let mut sessions = self.sessions.lock().await;
        for (key, tx) in sessions.drain() {
            if let Err(e) = tx.send(SessionEvent::Shutdown).await {
                warn!(session = %key, error = %e, "failed to send shutdown to session");
            }
        }
        info!("all sessions shut down");
    }

    /// Returns the number of active sessions.
    pub async fn session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// Build a [`SessionConfig`] for a new session.
    fn build_session_config(&self, session_id: String, user_id: i64) -> SessionConfig {
        let session_budget =
            SessionBudget::new(Arc::clone(&self.daily_budget), self.config.budget.clone());

        SessionConfig {
            session_id,
            user_id,
            router: Arc::clone(&self.router),
            tool_router: Arc::clone(&self.tool_router),
            memory: Arc::clone(&self.memory),
            budget: session_budget,
            approval_manager: Arc::clone(&self.approval_manager),
            policy_context: self.policy_context.clone(),
            telegram_tx: self.telegram_tx.clone(),
            config: Arc::clone(&self.config),
            agent_config: Arc::clone(&self.agent_config),
        }
    }
}
