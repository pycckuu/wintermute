//! Session persistence and crash recovery.
//!
//! The [`SessionManager`] persists session state to SQLite after each agent
//! turn and recovers sessions that were active when the process crashed.
//! Reads go directly through the pool; writes use direct queries since session
//! operations are low-frequency and don't need the memory writer actor.

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use tracing::{debug, info};

/// Manages session persistence and crash recovery.
///
/// Each running session is checkpointed after every agent turn so that
/// budget state survives restarts. On startup, sessions left in `active`
/// or `paused` status are flagged as crashed and can be recovered.
pub struct SessionManager {
    /// SQLite connection pool for direct reads and writes.
    db: SqlitePool,
}

/// A recovered session with its saved state.
#[derive(Debug, Clone)]
pub struct RestoredSession {
    /// Session identifier (e.g. "user_12345").
    pub session_id: String,
    /// Telegram user ID that owns this session.
    pub user_id: i64,
    /// Channel the session was running on (e.g. "telegram").
    pub channel: String,
    /// Tokens consumed before the crash.
    pub budget_tokens_used: u64,
    /// Whether the budget was paused at crash time.
    pub budget_paused: bool,
}

impl SessionManager {
    /// Create a new session manager backed by the given SQLite pool.
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    /// Create a new session record in SQLite.
    ///
    /// Called when a new session is spawned for a user. The session starts
    /// in `active` status with zero budget usage.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    pub async fn create_session(
        &self,
        session_id: &str,
        user_id: i64,
        channel: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO sessions (id, user_id, status, channel, budget_tokens_used, budget_paused, updated_at) \
             VALUES (?1, ?2, 'active', ?3, 0, FALSE, datetime('now'))",
        )
        .bind(session_id)
        .bind(user_id)
        .bind(channel)
        .execute(&self.db)
        .await
        .context("failed to create session record")?;

        debug!(session_id, user_id, channel, "session record created");
        Ok(())
    }

    /// Checkpoint session state after an agent turn.
    ///
    /// Updates the budget counters and paused flag so that a crash between
    /// turns loses at most one turn of progress.
    ///
    /// # Errors
    ///
    /// Returns an error if the database update fails.
    pub async fn checkpoint(&self, session_id: &str, tokens_used: u64, paused: bool) -> Result<()> {
        let tokens_i64 = i64::try_from(tokens_used).unwrap_or(i64::MAX);
        sqlx::query(
            "UPDATE sessions \
             SET budget_tokens_used = ?1, budget_paused = ?2, \
                 status = CASE WHEN ?2 THEN 'paused' ELSE 'active' END, \
                 updated_at = datetime('now') \
             WHERE id = ?3",
        )
        .bind(tokens_i64)
        .bind(paused)
        .bind(session_id)
        .execute(&self.db)
        .await
        .context("failed to checkpoint session")?;

        debug!(session_id, tokens_used, paused, "session checkpointed");
        Ok(())
    }

    /// Mark a session as completed.
    ///
    /// Called when a session is explicitly shut down (e.g. user sends `/reset`).
    ///
    /// # Errors
    ///
    /// Returns an error if the database update fails.
    pub async fn complete_session(&self, session_id: &str) -> Result<()> {
        sqlx::query(
            "UPDATE sessions \
             SET status = 'completed', completed_at = datetime('now'), \
                 updated_at = datetime('now') \
             WHERE id = ?1",
        )
        .bind(session_id)
        .execute(&self.db)
        .await
        .context("failed to complete session")?;

        debug!(session_id, "session marked completed");
        Ok(())
    }

    /// Mark all active/paused sessions as crashed.
    ///
    /// Called on startup before [`recover_sessions`](Self::recover_sessions)
    /// to flag sessions that were running when the process died. Sets
    /// `crash_reason` so they can be distinguished from newly created sessions
    /// while remaining queryable by `recover_sessions`.
    ///
    /// Returns the number of sessions marked as crashed.
    ///
    /// # Errors
    ///
    /// Returns an error if the database update fails.
    pub async fn mark_crashed_sessions(&self) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE sessions \
             SET crash_reason = 'process restart', \
                 updated_at = datetime('now') \
             WHERE status IN ('active', 'paused')",
        )
        .execute(&self.db)
        .await
        .context("failed to mark crashed sessions")?;

        let count = result.rows_affected();
        if count > 0 {
            info!(count, "marked crashed sessions");
        }
        Ok(count)
    }

    /// Find all sessions that were active or paused when the process died.
    ///
    /// Returns sessions that have a `crash_reason` set (by
    /// [`mark_crashed_sessions`](Self::mark_crashed_sessions)) and are still
    /// in `active` or `paused` status.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn recover_sessions(&self) -> Result<Vec<RestoredSession>> {
        let rows: Vec<(String, i64, String, i64, bool)> = sqlx::query_as(
            "SELECT id, user_id, channel, \
                    COALESCE(budget_tokens_used, 0), \
                    COALESCE(budget_paused, FALSE) \
             FROM sessions \
             WHERE status IN ('active', 'paused') AND crash_reason IS NOT NULL",
        )
        .fetch_all(&self.db)
        .await
        .context("failed to query crashed sessions")?;

        let sessions: Vec<RestoredSession> = rows
            .into_iter()
            .map(
                |(session_id, user_id, channel, tokens, paused)| RestoredSession {
                    session_id,
                    user_id,
                    channel,
                    budget_tokens_used: u64::try_from(tokens).unwrap_or(0),
                    budget_paused: paused,
                },
            )
            .collect();

        if !sessions.is_empty() {
            info!(count = sessions.len(), "found recoverable sessions");
        }

        Ok(sessions)
    }
}
