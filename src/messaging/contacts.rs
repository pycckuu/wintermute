//! Contact resolution and persistence.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tracing::trace;

use super::MessagingError;

/// Row type returned by SQLite queries for contacts.
type ContactRow = (
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// A contact the agent can communicate with on behalf of the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    /// Database ID (None for new contacts).
    pub id: Option<i64>,
    /// Display name.
    pub name: String,
    /// Phone number.
    pub phone: Option<String>,
    /// WhatsApp JID for direct messaging.
    pub whatsapp_jid: Option<String>,
    /// Organization or company.
    pub organization: Option<String>,
    /// Freeform notes.
    pub notes: Option<String>,
}

/// Insert or update a contact.
///
/// If `contact.id` is `Some`, updates the existing row. Otherwise inserts a new
/// row and returns the auto-generated ID.
///
/// # Errors
///
/// Returns [`MessagingError::Database`] on SQLite failure.
pub async fn upsert_contact(db: &SqlitePool, contact: &Contact) -> Result<i64, MessagingError> {
    if let Some(id) = contact.id {
        sqlx::query(
            "UPDATE contacts SET name=?1, phone=?2, whatsapp_jid=?3, \
             organization=?4, notes=?5 WHERE id=?6",
        )
        .bind(&contact.name)
        .bind(&contact.phone)
        .bind(&contact.whatsapp_jid)
        .bind(&contact.organization)
        .bind(&contact.notes)
        .bind(id)
        .execute(db)
        .await?;
        return Ok(id);
    }
    let result = sqlx::query(
        "INSERT INTO contacts (name, phone, whatsapp_jid, organization, notes) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(&contact.name)
    .bind(&contact.phone)
    .bind(&contact.whatsapp_jid)
    .bind(&contact.organization)
    .bind(&contact.notes)
    .execute(db)
    .await?;
    let id = result.last_insert_rowid();
    trace!(contact_id = id, name = %contact.name, "contact created");
    Ok(id)
}

/// Search contacts by name (case-insensitive LIKE match).
///
/// # Errors
///
/// Returns [`MessagingError::Database`] on SQLite failure.
pub async fn search_contacts(
    db: &SqlitePool,
    query: &str,
    limit: usize,
) -> Result<Vec<Contact>, MessagingError> {
    let pattern = format!("%{query}%");
    let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
    let rows: Vec<ContactRow> = sqlx::query_as(
        "SELECT id, name, phone, whatsapp_jid, organization, notes \
         FROM contacts WHERE name LIKE ?1 ORDER BY name LIMIT ?2",
    )
    .bind(&pattern)
    .bind(limit_i64)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, phone, jid, org, notes)| Contact {
            id: Some(id),
            name,
            phone,
            whatsapp_jid: jid,
            organization: org,
            notes,
        })
        .collect())
}

/// Load a contact by ID.
///
/// # Errors
///
/// Returns [`MessagingError::ContactNotFound`] if no contact matches,
/// or [`MessagingError::Database`] on SQLite failure.
pub async fn load_contact(db: &SqlitePool, contact_id: i64) -> Result<Contact, MessagingError> {
    let row: ContactRow = sqlx::query_as(
        "SELECT id, name, phone, whatsapp_jid, organization, notes \
         FROM contacts WHERE id = ?1",
    )
    .bind(contact_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| MessagingError::ContactNotFound(contact_id.to_string()))?;
    Ok(Contact {
        id: Some(row.0),
        name: row.1,
        phone: row.2,
        whatsapp_jid: row.3,
        organization: row.4,
        notes: row.5,
    })
}
