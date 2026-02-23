//! Memory engine with SQLite persistence, FTS5 search, and optional vector search.
//!
//! The [`MemoryEngine`] is the sole gateway to `memory.db`. All reads go through
//! [`MemoryEngine::search`] (concurrent). All writes go through a single-writer
//! actor backed by an [`mpsc`] channel to prevent SQLite
//! write contention.
//!
//! Embedding-based vector search is optional — when no embedding model is
//! configured, search falls back to FTS5 only.

pub mod embedder;
pub mod search;
pub mod writer;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::mpsc;
use tracing::{info, warn};

use self::embedder::Embedder;
use self::writer::WriteOp;

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Kind of memory stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    /// A declarative fact (e.g. "user prefers dark mode").
    Fact,
    /// A step-by-step procedure (e.g. "how to deploy the app").
    Procedure,
    /// A conversation episode summary.
    Episode,
    /// A reusable skill or tool reference.
    Skill,
}

impl MemoryKind {
    /// Returns the string representation stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Procedure => "procedure",
            Self::Episode => "episode",
            Self::Skill => "skill",
        }
    }

    /// Parse from a SQLite text value.
    ///
    /// # Errors
    ///
    /// Returns an error if the value is not a recognised kind.
    pub fn parse(s: &str) -> Result<Self, MemoryError> {
        match s {
            "fact" => Ok(Self::Fact),
            "procedure" => Ok(Self::Procedure),
            "episode" => Ok(Self::Episode),
            "skill" => Ok(Self::Skill),
            other => Err(MemoryError::InvalidEnum {
                field: "kind",
                value: other.to_owned(),
            }),
        }
    }
}

/// Status of a memory entry in the staged-learning pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryStatus {
    /// Active and included in search results.
    Active,
    /// Pending promotion from the observer pipeline.
    Pending,
    /// Archived — excluded from search results.
    Archived,
}

impl MemoryStatus {
    /// Returns the string representation stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Pending => "pending",
            Self::Archived => "archived",
        }
    }

    /// Parse from a SQLite text value.
    ///
    /// # Errors
    ///
    /// Returns an error if the value is not a recognised status.
    pub fn parse(s: &str) -> Result<Self, MemoryError> {
        match s {
            "active" => Ok(Self::Active),
            "pending" => Ok(Self::Pending),
            "archived" => Ok(Self::Archived),
            other => Err(MemoryError::InvalidEnum {
                field: "status",
                value: other.to_owned(),
            }),
        }
    }
}

/// Who approved a trust ledger entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustSource {
    /// Pre-approved via `config.toml` allowed_domains.
    Config,
    /// Approved by user at runtime.
    User,
}

impl TrustSource {
    /// Returns the string representation stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::User => "user",
        }
    }

    /// Parse from a SQLite text value.
    ///
    /// # Errors
    ///
    /// Returns an error if the value is not a recognised trust source.
    pub fn parse(s: &str) -> Result<Self, MemoryError> {
        match s {
            "config" => Ok(Self::Config),
            "user" => Ok(Self::User),
            other => Err(MemoryError::InvalidEnum {
                field: "approved_by",
                value: other.to_owned(),
            }),
        }
    }
}

/// Origin source that created a memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemorySource {
    /// Created by explicit user action.
    User,
    /// Extracted by the observer pipeline.
    Observer,
    /// Created by the agent via `memory_save` tool.
    Agent,
}

impl MemorySource {
    /// Returns the string representation stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Observer => "observer",
            Self::Agent => "agent",
        }
    }

    /// Parse from a SQLite text value.
    ///
    /// # Errors
    ///
    /// Returns an error if the value is not a recognised source.
    pub fn parse(s: &str) -> Result<Self, MemoryError> {
        match s {
            "user" => Ok(Self::User),
            "observer" => Ok(Self::Observer),
            "agent" => Ok(Self::Agent),
            other => Err(MemoryError::InvalidEnum {
                field: "source",
                value: other.to_owned(),
            }),
        }
    }
}

/// A memory entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    /// Database row id (`None` for entries not yet persisted).
    pub id: Option<i64>,
    /// Kind of memory.
    pub kind: MemoryKind,
    /// Text content.
    pub content: String,
    /// Optional JSON metadata (source, tags, related tool, etc.).
    pub metadata: Option<serde_json::Value>,
    /// Current pipeline status.
    pub status: MemoryStatus,
    /// Origin source.
    pub source: MemorySource,
    /// ISO-8601 creation timestamp (set by SQLite on insert).
    pub created_at: Option<String>,
    /// ISO-8601 last-update timestamp (set by SQLite on insert/update).
    pub updated_at: Option<String>,
}

/// A conversation log entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationEntry {
    /// Session identifier.
    pub session_id: String,
    /// Message role (`user`, `assistant`, `tool_call`, `tool_result`).
    pub role: String,
    /// Message content.
    pub content: String,
    /// Tokens consumed by this message (if known).
    pub tokens_used: Option<i32>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from memory engine operations.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// Database operation failed.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    /// Write channel is closed (writer actor stopped).
    #[error("memory writer channel closed")]
    WriterClosed,

    /// Embedding generation failed.
    #[error("embedding error: {0}")]
    Embedding(String),

    /// An invalid enum value was read from the database.
    #[error("invalid {field} value: {value:?}")]
    InvalidEnum {
        /// Which field contained the bad value.
        field: &'static str,
        /// The unexpected value.
        value: String,
    },

    /// Content exceeds the maximum allowed size.
    #[error("content too large: {size} bytes exceeds {max} byte limit")]
    ContentTooLarge {
        /// Actual content size in bytes.
        size: usize,
        /// Maximum allowed size.
        max: usize,
    },
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Writer channel capacity — bounded to provide backpressure.
const WRITER_CHANNEL_CAPACITY: usize = 1024;

/// Maximum allowed content size in bytes for a single memory or conversation entry.
///
/// Prevents unbounded storage from a single write. 64 KiB is generous for
/// text content while preventing accidental multi-megabyte entries.
pub const MAX_CONTENT_SIZE: usize = 64 * 1024;

/// Central memory engine managing persistence, search, and write serialization.
///
/// All reads go directly through the connection pool (concurrent).
/// All writes go through a single-writer actor via [`mpsc`].
pub struct MemoryEngine {
    /// Connection pool for reads.
    db: SqlitePool,
    /// Channel to the single-writer actor.
    writer_tx: mpsc::Sender<WriteOp>,
    /// Writer actor join handle (held so we can await on shutdown).
    writer_handle: tokio::task::JoinHandle<()>,
    /// Optional embedder for vector search.
    embedder: Option<Arc<dyn Embedder>>,
}

impl std::fmt::Debug for MemoryEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryEngine")
            .field("embedder", &self.embedder.is_some())
            .finish_non_exhaustive()
    }
}

impl MemoryEngine {
    /// Create a new memory engine backed by the given SQLite pool.
    ///
    /// Spawns the single-writer actor as a background Tokio task.
    /// If an [`Embedder`] is provided, vector search is enabled.
    pub async fn new(
        db: SqlitePool,
        embedder: Option<Arc<dyn Embedder>>,
    ) -> Result<Self, MemoryError> {
        let (writer_tx, writer_rx) = mpsc::channel(WRITER_CHANNEL_CAPACITY);
        let writer_pool = db.clone();
        let writer_handle = tokio::spawn(writer::run_writer(writer_pool, writer_rx));

        info!(embedder = embedder.is_some(), "memory engine initialised");

        Ok(Self {
            db,
            writer_tx,
            writer_handle,
            embedder,
        })
    }

    /// Search memories using FTS5 (and optionally vector similarity).
    ///
    /// Returns up to `limit` active memories ranked by relevance.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Memory>, MemoryError> {
        search::search(&self.db, self.embedder.as_deref(), query, limit).await
    }

    /// Persist a new memory entry.
    ///
    /// The entry is sent to the single-writer actor for serialized insertion.
    /// If an embedder is configured the embedding is computed before sending.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::WriterClosed`] if the writer actor has stopped.
    pub async fn save_memory(&self, mut memory: Memory) -> Result<(), MemoryError> {
        if memory.content.len() > MAX_CONTENT_SIZE {
            return Err(MemoryError::ContentTooLarge {
                size: memory.content.len(),
                max: MAX_CONTENT_SIZE,
            });
        }

        // Generate embedding if configured and not already present.
        if let Some(ref emb) = self.embedder {
            match emb.embed(&memory.content).await {
                Ok(embedding) => {
                    // Store embedding in metadata for now (sqlite-vec integration deferred).
                    let mut meta = memory
                        .metadata
                        .take()
                        .unwrap_or_else(|| serde_json::json!({}));
                    if let Some(obj) = meta.as_object_mut() {
                        obj.insert(
                            "embedding_dims".to_owned(),
                            serde_json::json!(embedding.len()),
                        );
                    }
                    memory.metadata = Some(meta);
                }
                Err(err) => {
                    warn!(error = %err, "embedding generation failed; saving without embedding");
                }
            }
        }
        self.writer_tx
            .send(WriteOp::SaveMemory(memory))
            .await
            .map_err(|_| MemoryError::WriterClosed)
    }

    /// Persist a conversation log entry.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::WriterClosed`] if the writer actor has stopped.
    pub async fn save_conversation(&self, entry: ConversationEntry) -> Result<(), MemoryError> {
        if entry.content.len() > MAX_CONTENT_SIZE {
            return Err(MemoryError::ContentTooLarge {
                size: entry.content.len(),
                max: MAX_CONTENT_SIZE,
            });
        }
        self.writer_tx
            .send(WriteOp::SaveConversation(entry))
            .await
            .map_err(|_| MemoryError::WriterClosed)
    }

    /// Update the status of an existing memory entry.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::WriterClosed`] if the writer actor has stopped.
    pub async fn update_memory_status(
        &self,
        id: i64,
        status: MemoryStatus,
    ) -> Result<(), MemoryError> {
        self.writer_tx
            .send(WriteOp::UpdateMemoryStatus { id, status })
            .await
            .map_err(|_| MemoryError::WriterClosed)
    }

    /// Search memories filtered by status, ordered by most recently updated.
    ///
    /// Returns up to `limit` memories with the given status.
    pub async fn search_by_status(
        &self,
        status: MemoryStatus,
        limit: usize,
    ) -> Result<Vec<Memory>, MemoryError> {
        search::search_by_status(&self.db, status.as_str(), limit).await
    }

    /// Delete a memory by its row id.
    ///
    /// The deletion is sent to the single-writer actor for serialized execution.
    /// Both the `memories` row and its corresponding FTS5 index entry are removed
    /// (via the `memories_ad` trigger).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::WriterClosed`] if the writer actor has stopped.
    pub async fn delete_memory(&self, id: i64) -> Result<(), MemoryError> {
        self.writer_tx
            .send(WriteOp::DeleteMemory { id })
            .await
            .map_err(|_| MemoryError::WriterClosed)
    }

    /// Count the number of memories with the given status.
    pub async fn count_by_status(&self, status: MemoryStatus) -> Result<u64, MemoryError> {
        let row: (i64,) = sqlx::query_as("SELECT count(*) FROM memories WHERE status = ?1")
            .bind(status.as_str())
            .fetch_one(&self.db)
            .await?;
        // count(*) is always non-negative, safe to cast.
        Ok(row.0.cast_unsigned())
    }

    /// Get the database file size in bytes for health reporting.
    ///
    /// Uses SQLite's `page_count * page_size` PRAGMA to compute the size.
    /// This works for both file-backed and in-memory databases.
    pub async fn db_size_bytes(&self) -> Result<u64, MemoryError> {
        let row: (i64,) = sqlx::query_as(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
        )
        .fetch_one(&self.db)
        .await
        .map_err(MemoryError::Database)?;
        // page_count * page_size is always non-negative, safe to cast.
        Ok(row.0.cast_unsigned())
    }

    /// Record a domain as trusted in the trust ledger.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::WriterClosed`] if the writer actor has stopped.
    pub async fn trust_domain(
        &self,
        domain: &str,
        approved_by: TrustSource,
    ) -> Result<(), MemoryError> {
        self.writer_tx
            .send(WriteOp::TrustDomain {
                domain: domain.to_owned(),
                approved_by,
            })
            .await
            .map_err(|_| MemoryError::WriterClosed)
    }

    /// Returns `true` if an embedding model is configured for vector search.
    pub fn has_embedder(&self) -> bool {
        self.embedder.is_some()
    }

    /// Check if a domain is trusted (exists in the trust ledger).
    pub async fn is_domain_trusted(&self, domain: &str) -> Result<bool, MemoryError> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM trust_ledger WHERE domain = ?1")
            .bind(domain)
            .fetch_optional(&self.db)
            .await?;
        Ok(row.is_some())
    }

    /// Search conversations by session id.
    pub async fn search_conversations(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<ConversationEntry>, MemoryError> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows: Vec<(String, String, String, Option<i32>)> = sqlx::query_as(
            "SELECT session_id, role, content, tokens_used \
             FROM conversations \
             WHERE session_id = ?1 \
             ORDER BY id ASC \
             LIMIT ?2",
        )
        .bind(session_id)
        .bind(limit_i64)
        .fetch_all(&self.db)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(sid, role, content, tokens)| ConversationEntry {
                session_id: sid,
                role,
                content,
                tokens_used: tokens,
            })
            .collect())
    }

    /// Returns a reference to the underlying SQLite pool (for migrations, etc.).
    pub fn pool(&self) -> &SqlitePool {
        &self.db
    }

    /// Gracefully shut down the writer actor.
    ///
    /// Drops the sender channel and awaits the writer task to drain.
    pub async fn shutdown(self) {
        drop(self.writer_tx);
        let _ = self.writer_handle.await;
        info!("memory engine shut down");
    }
}
