//! Outbound message audit logging.

use sqlx::SqlitePool;
use tracing::trace;

use super::MessagingError;

/// Log an outbound or inbound message to the audit trail.
///
/// All messages sent or received through the messaging subsystem are recorded
/// for compliance and debugging purposes.
///
/// # Errors
///
/// Returns [`MessagingError::Database`] on SQLite failure.
#[allow(clippy::too_many_arguments)]
pub async fn log_outbound(
    db: &SqlitePool,
    brief_id: Option<&str>,
    session_id: &str,
    channel: &str,
    recipient: &str,
    message_text: &str,
    direction: &str,
    redaction_warnings: Option<&str>,
    blocked: bool,
) -> Result<(), MessagingError> {
    sqlx::query(
        "INSERT INTO outbound_log (brief_id, session_id, channel, recipient, \
         message_text, direction, redaction_warnings, blocked) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )
    .bind(brief_id)
    .bind(session_id)
    .bind(channel)
    .bind(recipient)
    .bind(message_text)
    .bind(direction)
    .bind(redaction_warnings)
    .bind(blocked)
    .execute(db)
    .await?;

    trace!(channel, direction, blocked, "outbound message logged");
    Ok(())
}
