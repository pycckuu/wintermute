//! Route incoming WhatsApp messages to active brief sessions.
//!
//! When a message arrives from a WhatsApp contact, the router looks up the
//! contact by JID and finds the most recent active brief assigned to that
//! contact.

use sqlx::SqlitePool;
use tracing::{debug, info};

/// Result of routing an incoming message.
#[derive(Debug)]
pub enum RouteResult {
    /// Message matched an active brief.
    Routed {
        /// The brief ID that was matched.
        brief_id: String,
        /// The session ID associated with the brief.
        session_id: String,
    },
    /// No active brief found for this contact.
    Unhandled {
        /// The JID that could not be routed.
        jid: String,
    },
}

/// Route an incoming WhatsApp message to the appropriate brief session.
///
/// Looks up the contact by JID, finds the most recent active brief for
/// that contact, and returns the routing information.
pub async fn route_incoming(db: &SqlitePool, jid: &str) -> Result<RouteResult, sqlx::Error> {
    // Step 1: Find contact by WhatsApp JID
    let contact_row: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM contacts WHERE whatsapp_jid = ?1")
            .bind(jid)
            .fetch_optional(db)
            .await?;

    let contact_id = match contact_row {
        Some((id,)) => id,
        None => {
            debug!(jid, "no contact found for incoming WhatsApp message");
            return Ok(RouteResult::Unhandled {
                jid: jid.to_owned(),
            });
        }
    };

    // Step 2: Find the most recent active brief for this contact
    let brief_row: Option<(String, String)> = sqlx::query_as(
        "SELECT id, session_id FROM task_briefs \
         WHERE contact_id = ?1 AND status IN ('active', 'escalated', 'proposed') \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(contact_id)
    .fetch_optional(db)
    .await?;

    match brief_row {
        Some((brief_id, session_id)) => {
            info!(jid, %brief_id, "routed incoming message to brief");
            Ok(RouteResult::Routed {
                brief_id,
                session_id,
            })
        }
        None => {
            debug!(jid, contact_id, "contact found but no active brief");
            Ok(RouteResult::Unhandled {
                jid: jid.to_owned(),
            })
        }
    }
}
