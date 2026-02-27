//! Messaging module: task briefs, outbound composition, privacy redaction, and audit.
//!
//! # SQLite Write Pattern
//!
//! Unlike the memory engine (which uses a single-writer actor), messaging tables
//! (`task_briefs`, `contacts`, `outbound_log`) use direct pool writes. This is
//! acceptable because: (1) these tables are never written by the memory actor,
//! (2) SQLite WAL mode allows concurrent writes from different tables, and
//! (3) messaging writes are low-frequency (one per human-like delayed message).

pub mod audit;
pub mod brief;
pub mod contacts;
pub mod outbound_composer;
pub mod outbound_context;
pub mod outbound_redactor;

/// Errors from the messaging subsystem.
#[derive(Debug, thiserror::Error)]
pub enum MessagingError {
    /// Database operation failed.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    /// The requested brief was not found.
    #[error("brief not found: {0}")]
    BriefNotFound(String),

    /// State transition is not allowed.
    #[error("invalid state transition: {from} -> {to}")]
    InvalidTransition {
        /// The source state.
        from: String,
        /// The target state.
        to: String,
    },

    /// The requested contact was not found.
    #[error("contact not found: {0}")]
    ContactNotFound(String),

    /// Outbound message composition failed.
    #[error("composition failed: {0}")]
    CompositionFailed(String),

    /// Redactor blocked the outbound message.
    #[error("redaction blocked: {0}")]
    RedactionBlocked(String),
}
