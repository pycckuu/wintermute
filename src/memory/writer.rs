//! Single-writer actor for serialized SQLite writes.
//!
//! All database mutations flow through this actor via an
//! [`mpsc`](tokio::sync::mpsc) channel. This prevents SQLite write contention
//! while allowing concurrent reads through the connection pool.

use sqlx::SqlitePool;
use tokio::sync::mpsc;
use tracing::{error, trace};

use super::{ConversationEntry, Memory, MemoryStatus, TrustSource};

/// Operations that can be sent to the write actor.
#[derive(Debug)]
pub enum WriteOp {
    /// Persist a new memory.
    SaveMemory(Memory),

    /// Persist a conversation entry.
    SaveConversation(ConversationEntry),

    /// Update the status of an existing memory.
    UpdateMemoryStatus {
        /// Memory row id.
        id: i64,
        /// New status value.
        status: MemoryStatus,
    },

    /// Record a trusted domain in the trust ledger.
    TrustDomain {
        /// Domain name.
        domain: String,
        /// Who approved it.
        approved_by: TrustSource,
    },
}

/// Run the single-writer actor loop.
///
/// Processes [`WriteOp`] messages until the sender half is dropped.
/// Each operation is executed as an individual SQL statement.
pub async fn run_writer(db: SqlitePool, mut rx: mpsc::Receiver<WriteOp>) {
    while let Some(op) = rx.recv().await {
        if let Err(err) = handle_op(&db, &op).await {
            error!(?op, error = %err, "memory write failed");
        }
    }
    trace!("memory writer actor stopped");
}

async fn handle_op(db: &SqlitePool, op: &WriteOp) -> Result<(), sqlx::Error> {
    match op {
        WriteOp::SaveMemory(memory) => {
            let metadata_str = memory.metadata.as_ref().map(|v| v.to_string());
            sqlx::query(
                "INSERT INTO memories (kind, content, metadata, status, source) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind(memory.kind.as_str())
            .bind(&memory.content)
            .bind(&metadata_str)
            .bind(memory.status.as_str())
            .bind(memory.source.as_str())
            .execute(db)
            .await?;
            trace!(kind = memory.kind.as_str(), "memory saved");
        }

        WriteOp::SaveConversation(entry) => {
            sqlx::query(
                "INSERT INTO conversations (session_id, role, content, tokens_used) \
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(&entry.session_id)
            .bind(&entry.role)
            .bind(&entry.content)
            .bind(entry.tokens_used)
            .execute(db)
            .await?;
            trace!(session = %entry.session_id, role = %entry.role, "conversation saved");
        }

        WriteOp::UpdateMemoryStatus { id, status } => {
            sqlx::query(
                "UPDATE memories SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
            )
            .bind(status.as_str())
            .bind(id)
            .execute(db)
            .await?;
            trace!(id, status = status.as_str(), "memory status updated");
        }

        WriteOp::TrustDomain {
            domain,
            approved_by,
        } => {
            sqlx::query("INSERT OR IGNORE INTO trust_ledger (domain, approved_by) VALUES (?1, ?2)")
                .bind(domain)
                .bind(approved_by.as_str())
                .execute(db)
                .await?;
            trace!(domain, approved_by = approved_by.as_str(), "domain trusted");
        }
    }
    Ok(())
}
