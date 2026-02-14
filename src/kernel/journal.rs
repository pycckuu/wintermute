//! Task journal for session persistence and adapter state (persistence-recovery spec §2, §3).
//!
//! Persists adapter state (e.g. Telegram offset), conversation history,
//! and session working memory to SQLite so the kernel can resume context
//! after restart. No task lifecycle journaling — if the kernel crashes
//! mid-task, the user simply re-asks.

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use thiserror::Error;
use uuid::Uuid;

use crate::types::SecurityLabel;

/// (principal, service, vault_key, expected_prefix) — returned by
/// [`TaskJournal::load_all_pending_credential_prompts`].
pub type PendingPromptRow = (String, String, String, Option<String>);

// ── Errors ──────────────────────────────────────────────────────

/// Journal operation errors (persistence-recovery spec §2).
#[derive(Debug, Error)]
pub enum JournalError {
    /// SQLite database error.
    #[error("database error: {0}")]
    Database(String),
    /// Serialization/deserialization error.
    #[error("serialization error: {0}")]
    Serialization(String),
}

impl From<rusqlite::Error> for JournalError {
    fn from(e: rusqlite::Error) -> Self {
        JournalError::Database(e.to_string())
    }
}

impl From<serde_json::Error> for JournalError {
    fn from(e: serde_json::Error) -> Self {
        JournalError::Serialization(e.to_string())
    }
}

// ── Types ───────────────────────────────────────────────────────

/// A long-term memory entry from the memories table (memory spec §3).
#[derive(Debug, Clone)]
pub struct MemoryRow {
    /// Unique memory ID (UUID string).
    pub id: String,
    /// Memory content text.
    pub content: String,
    /// Security label of this memory entry (spec 4.3).
    pub label: SecurityLabel,
    /// Source: "explicit" or "consolidated".
    pub source: String,
    /// When the memory was created.
    pub created_at: DateTime<Utc>,
    /// Task ID that created this memory (optional).
    pub task_id: Option<String>,
}

/// A row from the working_memory table (spec 9.1).
#[derive(Debug, Clone)]
pub struct WorkingMemoryRow {
    /// Task UUID.
    pub task_id: Uuid,
    /// When the task completed.
    pub timestamp: DateTime<Utc>,
    /// Short summary of what was requested.
    pub request_summary: String,
    /// JSON-serialized tool outputs.
    pub tool_outputs_json: String,
    /// Short summary of the response sent.
    pub response_summary: String,
    /// Highest security label touched.
    pub label: SecurityLabel,
}

/// Parameters for saving a working memory result (spec 9.1).
#[derive(Debug, Clone)]
pub struct SaveWorkingMemoryParams<'a> {
    /// JSON-serialized principal key.
    pub principal: &'a str,
    /// Task UUID.
    pub task_id: Uuid,
    /// When the task completed.
    pub timestamp: &'a DateTime<Utc>,
    /// Short summary of what was requested.
    pub request_summary: &'a str,
    /// JSON-serialized tool outputs.
    pub tool_outputs_json: &'a str,
    /// Short summary of the response sent.
    pub response_summary: &'a str,
    /// Highest security label touched.
    pub label: SecurityLabel,
}

// ── Helpers ─────────────────────────────────────────────────────

/// Serialize a `SecurityLabel` to string for SQLite storage.
fn label_to_str(label: SecurityLabel) -> &'static str {
    match label {
        SecurityLabel::Public => "public",
        SecurityLabel::Internal => "internal",
        SecurityLabel::Sensitive => "sensitive",
        SecurityLabel::Regulated => "regulated",
        SecurityLabel::Secret => "secret",
    }
}

/// Deserialize a `SecurityLabel` from string.
fn str_to_label(s: &str) -> SecurityLabel {
    match s {
        "internal" => SecurityLabel::Internal,
        "sensitive" => SecurityLabel::Sensitive,
        "regulated" => SecurityLabel::Regulated,
        "secret" => SecurityLabel::Secret,
        _ => SecurityLabel::Public,
    }
}

/// Convert `SecurityLabel` to integer for SQL comparison (spec 4.3).
///
/// Ordered: public(0) < internal(1) < sensitive(2) < regulated(3) < secret(4).
fn label_to_int(label: SecurityLabel) -> i32 {
    match label {
        SecurityLabel::Public => 0,
        SecurityLabel::Internal => 1,
        SecurityLabel::Sensitive => 2,
        SecurityLabel::Regulated => 3,
        SecurityLabel::Secret => 4,
    }
}

/// Parse an RFC 3339 timestamp or return now.
fn parse_rfc3339_or_now(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

// ── SQL Schema ──────────────────────────────────────────────────

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS adapter_state (
    adapter     TEXT PRIMARY KEY,
    state_json  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS conversation_turns (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    principal   TEXT NOT NULL,
    role        TEXT NOT NULL,
    summary     TEXT NOT NULL,
    timestamp   TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_conversation_turns_principal ON conversation_turns(principal);

CREATE TABLE IF NOT EXISTS working_memory (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    principal        TEXT NOT NULL,
    task_id          TEXT NOT NULL,
    timestamp        TEXT NOT NULL,
    request_summary  TEXT NOT NULL,
    tool_outputs     TEXT NOT NULL,
    response_summary TEXT NOT NULL,
    label            TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_working_memory_principal ON working_memory(principal);

CREATE TABLE IF NOT EXISTS persona (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS memories (
    id          TEXT PRIMARY KEY,
    content     TEXT NOT NULL,
    label       TEXT NOT NULL DEFAULT 'internal',
    source      TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    task_id     TEXT
);

CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
    content,
    content='memories',
    content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
    INSERT INTO memories_fts(rowid, content)
    VALUES (new.rowid, new.content);
END;

CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, content)
    VALUES ('delete', old.rowid, old.content);
END;

CREATE TABLE IF NOT EXISTS pending_credential_prompts (
    principal        TEXT PRIMARY KEY,
    service          TEXT NOT NULL,
    vault_key        TEXT NOT NULL,
    expected_prefix  TEXT,
    created_at       TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS pending_message_deletions (
    chat_id    TEXT NOT NULL,
    message_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (chat_id, message_id)
);
"#;

// ── TaskJournal ─────────────────────────────────────────────────

/// SQLite-backed journal for adapter state and session persistence
/// (persistence-recovery spec §2, §3).
///
/// All methods take `&self` and use an internal `Mutex<Connection>`.
/// Writes are synchronous (rusqlite is sync) but fast (<1ms for typical ops).
pub struct TaskJournal {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for TaskJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskJournal").finish()
    }
}

impl TaskJournal {
    /// Open a journal backed by a file (persistence-recovery spec §2).
    pub fn open(path: &str) -> Result<Self, JournalError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory journal for testing.
    pub fn open_in_memory() -> Result<Self, JournalError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // ── Adapter state CRUD (persistence-recovery spec §2) ────────

    /// Save adapter-specific state (e.g. Telegram offset).
    pub fn save_adapter_state(&self, adapter: &str, state_json: &str) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO adapter_state (adapter, state_json, updated_at) VALUES (?1, ?2, ?3)",
            params![adapter, state_json, now],
        )?;
        Ok(())
    }

    /// Load adapter-specific state.
    pub fn load_adapter_state(&self, adapter: &str) -> Result<Option<String>, JournalError> {
        use rusqlite::OptionalExtension;
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.query_row(
            "SELECT state_json FROM adapter_state WHERE adapter = ?1",
            params![adapter],
            |row| row.get(0),
        )
        .optional()
        .map_err(JournalError::from)
    }

    /// Delete adapter state.
    pub fn delete_adapter_state(&self, adapter: &str) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.execute(
            "DELETE FROM adapter_state WHERE adapter = ?1",
            params![adapter],
        )?;
        Ok(())
    }

    // ── Session persistence (spec 9.1, 9.2) ──────────────────────

    /// Save a conversation turn for a principal (spec 9.2).
    pub fn save_conversation_turn(
        &self,
        principal: &str,
        role: &str,
        summary: &str,
        timestamp: &DateTime<Utc>,
    ) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.execute(
            "INSERT INTO conversation_turns (principal, role, summary, timestamp) VALUES (?1, ?2, ?3, ?4)",
            params![principal, role, summary, timestamp.to_rfc3339()],
        )?;
        Ok(())
    }

    /// Load the most recent conversation turns for a principal (spec 9.2).
    ///
    /// Returns up to `limit` turns ordered oldest-first (chronological).
    pub fn load_conversation_turns(
        &self,
        principal: &str,
        limit: usize,
    ) -> Result<Vec<(String, String, DateTime<Utc>)>, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = conn.prepare(
            "SELECT role, summary, timestamp FROM conversation_turns
             WHERE principal = ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![principal, limit_i64], |row| {
            let role: String = row.get(0)?;
            let summary: String = row.get(1)?;
            let ts_str: String = row.get(2)?;
            let ts = parse_rfc3339_or_now(&ts_str);
            Ok((role, summary, ts))
        })?;
        let mut turns = Vec::new();
        for row in rows {
            turns.push(row?);
        }
        // Reverse to get chronological order (query was DESC).
        turns.reverse();
        Ok(turns)
    }

    /// Save a working memory result for a principal (spec 9.1).
    pub fn save_working_memory_result(
        &self,
        params: &SaveWorkingMemoryParams<'_>,
    ) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.execute(
            "INSERT INTO working_memory (principal, task_id, timestamp, request_summary, tool_outputs, response_summary, label)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                params.principal,
                params.task_id.to_string(),
                params.timestamp.to_rfc3339(),
                params.request_summary,
                params.tool_outputs_json,
                params.response_summary,
                label_to_str(params.label),
            ],
        )?;
        Ok(())
    }

    /// Load the most recent working memory results for a principal (spec 9.1).
    ///
    /// Returns up to `limit` results ordered oldest-first (chronological).
    pub fn load_working_memory_results(
        &self,
        principal: &str,
        limit: usize,
    ) -> Result<Vec<WorkingMemoryRow>, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = conn.prepare(
            "SELECT task_id, timestamp, request_summary, tool_outputs, response_summary, label
             FROM working_memory
             WHERE principal = ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![principal, limit_i64], |row| {
            let task_id_str: String = row.get(0)?;
            let ts_str: String = row.get(1)?;
            let request_summary: String = row.get(2)?;
            let tool_outputs_json: String = row.get(3)?;
            let response_summary: String = row.get(4)?;
            let label_str: String = row.get(5)?;
            Ok(WorkingMemoryRow {
                task_id: Uuid::parse_str(&task_id_str).unwrap_or(Uuid::nil()),
                timestamp: parse_rfc3339_or_now(&ts_str),
                request_summary,
                tool_outputs_json,
                response_summary,
                label: str_to_label(&label_str),
            })
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        // Reverse to get chronological order (query was DESC).
        results.reverse();
        Ok(results)
    }

    /// Get all distinct principals that have session data (spec 9).
    pub fn load_session_principals(&self) -> Result<Vec<String>, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT principal FROM conversation_turns
             UNION
             SELECT DISTINCT principal FROM working_memory",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut principals = Vec::new();
        for row in rows {
            principals.push(row?);
        }
        Ok(principals)
    }

    /// Trim conversation turns for a principal to keep at most `max` entries (spec 9.2).
    pub fn trim_conversation_turns(&self, principal: &str, max: usize) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let max_i64 = i64::try_from(max).unwrap_or(i64::MAX);
        conn.execute(
            "DELETE FROM conversation_turns WHERE principal = ?1 AND id NOT IN (
                SELECT id FROM conversation_turns WHERE principal = ?1 ORDER BY id DESC LIMIT ?2
            )",
            params![principal, max_i64],
        )?;
        Ok(())
    }

    /// Trim working memory results for a principal to keep at most `max` entries (spec 9.1).
    pub fn trim_working_memory(&self, principal: &str, max: usize) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let max_i64 = i64::try_from(max).unwrap_or(i64::MAX);
        conn.execute(
            "DELETE FROM working_memory WHERE principal = ?1 AND id NOT IN (
                SELECT id FROM working_memory WHERE principal = ?1 ORDER BY id DESC LIMIT ?2
            )",
            params![principal, max_i64],
        )?;
        Ok(())
    }

    // ── Persona CRUD (persona-onboarding spec §1) ─────────────────

    /// Get the persona string (persona-onboarding spec §1).
    pub fn get_persona(&self) -> Result<Option<String>, JournalError> {
        use rusqlite::OptionalExtension;
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.query_row(
            "SELECT value FROM persona WHERE key = 'persona'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(JournalError::from)
    }

    /// Set the persona string (persona-onboarding spec §1).
    pub fn set_persona(&self, value: &str) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO persona (key, value) VALUES ('persona', ?1)",
            params![value],
        )?;
        Ok(())
    }

    // ── Long-term memory CRUD (memory spec §3, §4, §6) ───────────

    /// Save a long-term memory entry (memory spec §4).
    pub fn save_memory(&self, row: &MemoryRow) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO memories (id, content, label, source, created_at, task_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                row.id,
                row.content,
                label_to_str(row.label),
                row.source,
                row.created_at.to_rfc3339(),
                row.task_id,
            ],
        )?;
        Ok(())
    }

    /// Search long-term memories via FTS5, filtered by label ceiling (memory spec §6).
    ///
    /// Returns up to `limit` results ranked by FTS5 relevance. Enforces
    /// No Read Up: only returns entries where `label <= label_ceiling` (spec 4.3).
    /// Returns an empty vec if query is empty (FTS5 errors on empty MATCH).
    pub fn search_memories(
        &self,
        query: &str,
        label_ceiling: SecurityLabel,
        limit: usize,
    ) -> Result<Vec<MemoryRow>, JournalError> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(vec![]);
        }

        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;

        let ceiling_int = label_to_int(label_ceiling);
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);

        let mut stmt = conn.prepare(
            "SELECT m.id, m.content, m.label, m.source, m.created_at, m.task_id
             FROM memories m
             JOIN memories_fts ON memories_fts.rowid = m.rowid
             WHERE memories_fts MATCH ?1
               AND (CASE m.label
                    WHEN 'public' THEN 0
                    WHEN 'internal' THEN 1
                    WHEN 'sensitive' THEN 2
                    WHEN 'regulated' THEN 3
                    WHEN 'secret' THEN 4
                    ELSE 5
                    END) <= ?2
             ORDER BY memories_fts.rank
             LIMIT ?3",
        )?;

        let rows = stmt.query_map(params![trimmed, ceiling_int, limit_i64], |row| {
            let id: String = row.get(0)?;
            let content: String = row.get(1)?;
            let label_str: String = row.get(2)?;
            let source: String = row.get(3)?;
            let created_at_str: String = row.get(4)?;
            let task_id: Option<String> = row.get(5)?;
            Ok(MemoryRow {
                id,
                content,
                label: str_to_label(&label_str),
                source,
                created_at: parse_rfc3339_or_now(&created_at_str),
                task_id,
            })
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    // ── Pending credential prompts (credential-acquisition spec §6) ──

    /// Save a pending credential prompt for a principal (credential-acquisition spec §6).
    pub fn save_pending_credential_prompt(
        &self,
        principal: &str,
        service: &str,
        vault_key: &str,
        expected_prefix: Option<&str>,
    ) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO pending_credential_prompts (principal, service, vault_key, expected_prefix, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![principal, service, vault_key, expected_prefix, now],
        )?;
        Ok(())
    }

    /// Load a pending credential prompt for a principal (credential-acquisition spec §6).
    pub fn load_pending_credential_prompt(
        &self,
        principal: &str,
    ) -> Result<Option<(String, String, Option<String>)>, JournalError> {
        use rusqlite::OptionalExtension;
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.query_row(
            "SELECT service, vault_key, expected_prefix FROM pending_credential_prompts WHERE principal = ?1",
            params![principal],
            |row| {
                let service: String = row.get(0)?;
                let vault_key: String = row.get(1)?;
                let expected_prefix: Option<String> = row.get(2)?;
                Ok((service, vault_key, expected_prefix))
            },
        )
        .optional()
        .map_err(JournalError::from)
    }

    /// Delete a pending credential prompt for a principal (credential-acquisition spec §6).
    pub fn delete_pending_credential_prompt(&self, principal: &str) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.execute(
            "DELETE FROM pending_credential_prompts WHERE principal = ?1",
            params![principal],
        )?;
        Ok(())
    }

    /// Load all pending credential prompts (credential-acquisition spec §6).
    ///
    /// Used on startup to restore the CredentialGate's in-memory state.
    pub fn load_all_pending_credential_prompts(
        &self,
    ) -> Result<Vec<PendingPromptRow>, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT principal, service, vault_key, expected_prefix FROM pending_credential_prompts",
        )?;
        let rows = stmt.query_map([], |row| {
            let principal: String = row.get(0)?;
            let service: String = row.get(1)?;
            let vault_key: String = row.get(2)?;
            let expected_prefix: Option<String> = row.get(3)?;
            Ok((principal, service, vault_key, expected_prefix))
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    // ── Pending message deletions (credential-acquisition spec §8) ──

    /// Save a pending message deletion (credential-acquisition spec §8).
    pub fn save_pending_deletion(
        &self,
        chat_id: &str,
        message_id: &str,
    ) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO pending_message_deletions (chat_id, message_id, created_at)
             VALUES (?1, ?2, ?3)",
            params![chat_id, message_id, now],
        )?;
        Ok(())
    }

    /// Delete a pending message deletion record (credential-acquisition spec §8).
    pub fn delete_pending_deletion(
        &self,
        chat_id: &str,
        message_id: &str,
    ) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.execute(
            "DELETE FROM pending_message_deletions WHERE chat_id = ?1 AND message_id = ?2",
            params![chat_id, message_id],
        )?;
        Ok(())
    }

    /// Load all pending message deletions (credential-acquisition spec §8).
    ///
    /// Used on startup to retry deletions that failed before shutdown.
    pub fn load_all_pending_deletions(&self) -> Result<Vec<(String, String)>, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let mut stmt = conn.prepare("SELECT chat_id, message_id FROM pending_message_deletions")?;
        let rows = stmt.query_map([], |row| {
            let chat_id: String = row.get(0)?;
            let message_id: String = row.get(1)?;
            Ok((chat_id, message_id))
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_journal() -> TaskJournal {
        TaskJournal::open_in_memory().expect("failed to create in-memory journal")
    }

    // ── Adapter state tests ─────────────────────────────────────

    #[test]
    fn test_adapter_state_crud() {
        let j = make_journal();
        j.save_adapter_state("telegram", r#"{"last_offset":12345}"#)
            .expect("save");

        let state = j
            .load_adapter_state("telegram")
            .expect("load")
            .expect("should exist");
        assert_eq!(state, r#"{"last_offset":12345}"#);

        // Overwrite.
        j.save_adapter_state("telegram", r#"{"last_offset":99999}"#)
            .expect("save");
        let state2 = j
            .load_adapter_state("telegram")
            .expect("load")
            .expect("should exist");
        assert_eq!(state2, r#"{"last_offset":99999}"#);

        // Delete.
        j.delete_adapter_state("telegram").expect("delete");
        let state3 = j.load_adapter_state("telegram").expect("load");
        assert!(state3.is_none());
    }

    #[test]
    fn test_adapter_state_not_found() {
        let j = make_journal();
        let state = j.load_adapter_state("nonexistent").expect("load");
        assert!(state.is_none());
    }

    // ── Session persistence tests ──────────────────────────────────

    #[test]
    fn test_conversation_turn_save_and_load() {
        let j = make_journal();
        let now = Utc::now();
        j.save_conversation_turn("\"Owner\"", "user", "check my email", &now)
            .expect("save user turn");
        j.save_conversation_turn("\"Owner\"", "assistant", "You have 2 emails", &now)
            .expect("save assistant turn");

        let turns = j.load_conversation_turns("\"Owner\"", 20).expect("load");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].0, "user");
        assert_eq!(turns[0].1, "check my email");
        assert_eq!(turns[1].0, "assistant");
        assert_eq!(turns[1].1, "You have 2 emails");
    }

    #[test]
    fn test_conversation_turns_limit() {
        let j = make_journal();
        let now = Utc::now();
        for i in 0..10 {
            j.save_conversation_turn("\"Owner\"", "user", &format!("turn {i}"), &now)
                .expect("save");
        }

        // Load only 3 most recent.
        let turns = j.load_conversation_turns("\"Owner\"", 3).expect("load");
        assert_eq!(turns.len(), 3);
        // Should be the last 3 in chronological order.
        assert_eq!(turns[0].1, "turn 7");
        assert_eq!(turns[1].1, "turn 8");
        assert_eq!(turns[2].1, "turn 9");
    }

    #[test]
    fn test_conversation_turns_isolation() {
        let j = make_journal();
        let now = Utc::now();
        j.save_conversation_turn("\"Owner\"", "user", "owner message", &now)
            .expect("save");
        j.save_conversation_turn("{\"TelegramPeer\":\"12345\"}", "user", "peer message", &now)
            .expect("save");

        let owner_turns = j.load_conversation_turns("\"Owner\"", 20).expect("load");
        assert_eq!(owner_turns.len(), 1);
        assert_eq!(owner_turns[0].1, "owner message");

        let peer_turns = j
            .load_conversation_turns("{\"TelegramPeer\":\"12345\"}", 20)
            .expect("load");
        assert_eq!(peer_turns.len(), 1);
        assert_eq!(peer_turns[0].1, "peer message");
    }

    #[test]
    fn test_working_memory_save_and_load() {
        let j = make_journal();
        let now = Utc::now();
        let task_id = Uuid::new_v4();
        let outputs =
            r#"[{"tool":"email","action":"list","output":{"count":5},"label":"sensitive"}]"#;
        j.save_working_memory_result(&SaveWorkingMemoryParams {
            principal: "\"Owner\"",
            task_id,
            timestamp: &now,
            request_summary: "check email",
            tool_outputs_json: outputs,
            response_summary: "Listed 5 emails",
            label: SecurityLabel::Sensitive,
        })
        .expect("save");

        let results = j
            .load_working_memory_results("\"Owner\"", 10)
            .expect("load");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].task_id, task_id);
        assert_eq!(results[0].request_summary, "check email");
        assert_eq!(results[0].response_summary, "Listed 5 emails");
        assert_eq!(results[0].label, SecurityLabel::Sensitive);
    }

    #[test]
    fn test_working_memory_limit() {
        let j = make_journal();
        let now = Utc::now();
        for i in 0..5 {
            let req = format!("task {i}");
            let resp = format!("result {i}");
            j.save_working_memory_result(&SaveWorkingMemoryParams {
                principal: "\"Owner\"",
                task_id: Uuid::new_v4(),
                timestamp: &now,
                request_summary: &req,
                tool_outputs_json: "[]",
                response_summary: &resp,
                label: SecurityLabel::Public,
            })
            .expect("save");
        }

        let results = j.load_working_memory_results("\"Owner\"", 2).expect("load");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].request_summary, "task 3");
        assert_eq!(results[1].request_summary, "task 4");
    }

    #[test]
    fn test_session_principals() {
        let j = make_journal();
        let now = Utc::now();
        j.save_conversation_turn("\"Owner\"", "user", "hello", &now)
            .expect("save");
        j.save_working_memory_result(&SaveWorkingMemoryParams {
            principal: "{\"TelegramPeer\":\"12345\"}",
            task_id: Uuid::new_v4(),
            timestamp: &now,
            request_summary: "task",
            tool_outputs_json: "[]",
            response_summary: "result",
            label: SecurityLabel::Public,
        })
        .expect("save");

        let principals = j.load_session_principals().expect("load");
        assert_eq!(principals.len(), 2);
        assert!(principals.contains(&"\"Owner\"".to_owned()));
        assert!(principals.contains(&"{\"TelegramPeer\":\"12345\"}".to_owned()));
    }

    #[test]
    fn test_trim_conversation_turns() {
        let j = make_journal();
        let now = Utc::now();
        for i in 0..10 {
            j.save_conversation_turn("\"Owner\"", "user", &format!("turn {i}"), &now)
                .expect("save");
        }

        j.trim_conversation_turns("\"Owner\"", 3).expect("trim");

        let turns = j.load_conversation_turns("\"Owner\"", 20).expect("load");
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].1, "turn 7");
    }

    #[test]
    fn test_trim_working_memory() {
        let j = make_journal();
        let now = Utc::now();
        for i in 0..5 {
            let req = format!("task {i}");
            let resp = format!("result {i}");
            j.save_working_memory_result(&SaveWorkingMemoryParams {
                principal: "\"Owner\"",
                task_id: Uuid::new_v4(),
                timestamp: &now,
                request_summary: &req,
                tool_outputs_json: "[]",
                response_summary: &resp,
                label: SecurityLabel::Public,
            })
            .expect("save");
        }

        j.trim_working_memory("\"Owner\"", 2).expect("trim");

        let results = j
            .load_working_memory_results("\"Owner\"", 10)
            .expect("load");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].request_summary, "task 3");
    }

    // ── Persona tests ───────────────────────────────────────────

    #[test]
    fn test_persona_get_returns_none_when_empty() {
        let j = make_journal();
        let persona = j.get_persona().expect("get");
        assert!(persona.is_none(), "fresh journal should have no persona");
    }

    #[test]
    fn test_persona_set_and_get() {
        let j = make_journal();
        j.set_persona("Name: Atlas. Owner: Igor. Style: concise.")
            .expect("set");

        let persona = j.get_persona().expect("get").expect("should exist");
        assert_eq!(persona, "Name: Atlas. Owner: Igor. Style: concise.");
    }

    #[test]
    fn test_persona_overwrite() {
        let j = make_journal();
        j.set_persona("__pending__").expect("set");
        j.set_persona("Name: Nova. Style: detailed.")
            .expect("overwrite");

        let persona = j.get_persona().expect("get").expect("should exist");
        assert_eq!(persona, "Name: Nova. Style: detailed.");
    }

    #[test]
    fn test_empty_session_load() {
        let j = make_journal();
        let turns = j.load_conversation_turns("\"Owner\"", 20).expect("load");
        assert!(turns.is_empty());
        let results = j
            .load_working_memory_results("\"Owner\"", 10)
            .expect("load");
        assert!(results.is_empty());
        let principals = j.load_session_principals().expect("load");
        assert!(principals.is_empty());
    }

    // ── Long-term memory tests ──────────────────────────────────

    fn make_memory(content: &str, label: SecurityLabel) -> MemoryRow {
        MemoryRow {
            id: Uuid::new_v4().to_string(),
            content: content.to_owned(),
            label,
            source: "explicit".to_owned(),
            created_at: Utc::now(),
            task_id: None,
        }
    }

    #[test]
    fn test_memory_save_and_search() {
        let j = make_journal();
        let row = make_memory("Flight to Bali on March 15th", SecurityLabel::Sensitive);
        j.save_memory(&row).expect("save");

        let results = j
            .search_memories("Bali", SecurityLabel::Sensitive, 10)
            .expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "Flight to Bali on March 15th");
        assert_eq!(results[0].label, SecurityLabel::Sensitive);
        assert_eq!(results[0].source, "explicit");
    }

    #[test]
    fn test_memory_label_filtering() {
        let j = make_journal();
        // Save a sensitive memory.
        let row = make_memory(
            "Private doctor appointment Tuesday",
            SecurityLabel::Sensitive,
        );
        j.save_memory(&row).expect("save");

        // Search with internal ceiling — should NOT find sensitive memory.
        let results = j
            .search_memories("doctor", SecurityLabel::Internal, 10)
            .expect("search");
        assert!(
            results.is_empty(),
            "internal ceiling should not see sensitive memories"
        );

        // Search with sensitive ceiling — should find it.
        let results = j
            .search_memories("doctor", SecurityLabel::Sensitive, 10)
            .expect("search");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_memory_empty_query() {
        let j = make_journal();
        let row = make_memory("Some fact", SecurityLabel::Internal);
        j.save_memory(&row).expect("save");

        // Empty query returns empty vec (guard, not FTS5 error).
        let results = j
            .search_memories("", SecurityLabel::Sensitive, 10)
            .expect("search");
        assert!(results.is_empty());

        let results = j
            .search_memories("   ", SecurityLabel::Sensitive, 10)
            .expect("search");
        assert!(results.is_empty());
    }

    #[test]
    fn test_memory_no_results() {
        let j = make_journal();
        let row = make_memory("Flight to Bali", SecurityLabel::Internal);
        j.save_memory(&row).expect("save");

        let results = j
            .search_memories("Tokyo", SecurityLabel::Sensitive, 10)
            .expect("search");
        assert!(results.is_empty());
    }

    #[test]
    fn test_memory_multiple_results_limited() {
        let j = make_journal();
        j.save_memory(&make_memory(
            "Meeting with Sarah Monday",
            SecurityLabel::Internal,
        ))
        .expect("save");
        j.save_memory(&make_memory(
            "Meeting with Alex Tuesday",
            SecurityLabel::Internal,
        ))
        .expect("save");
        j.save_memory(&make_memory(
            "Meeting with Bob Wednesday",
            SecurityLabel::Internal,
        ))
        .expect("save");

        // Limit to 2 results.
        let results = j
            .search_memories("Meeting", SecurityLabel::Sensitive, 2)
            .expect("search");
        assert_eq!(results.len(), 2);
    }

    // ── Pending credential prompt tests (credential-acquisition spec §6) ──

    #[test]
    fn test_pending_credential_prompt_roundtrip() {
        let j = make_journal();
        j.save_pending_credential_prompt("\"Owner\"", "notion", "vault:notion_token", Some("ntn_"))
            .expect("save");

        let prompt = j
            .load_pending_credential_prompt("\"Owner\"")
            .expect("load")
            .expect("should exist");
        assert_eq!(prompt.0, "notion");
        assert_eq!(prompt.1, "vault:notion_token");
        assert_eq!(prompt.2.as_deref(), Some("ntn_"));

        j.delete_pending_credential_prompt("\"Owner\"")
            .expect("delete");
        let deleted = j.load_pending_credential_prompt("\"Owner\"").expect("load");
        assert!(deleted.is_none());
    }

    #[test]
    fn test_pending_credential_prompt_no_prefix() {
        let j = make_journal();
        j.save_pending_credential_prompt("\"Owner\"", "custom_api", "vault:custom_token", None)
            .expect("save");

        let prompt = j
            .load_pending_credential_prompt("\"Owner\"")
            .expect("load")
            .expect("should exist");
        assert_eq!(prompt.0, "custom_api");
        assert!(prompt.2.is_none());
    }

    #[test]
    fn test_load_all_pending_credential_prompts() {
        let j = make_journal();
        j.save_pending_credential_prompt("\"Owner\"", "notion", "vault:notion_token", Some("ntn_"))
            .expect("save");
        j.save_pending_credential_prompt(
            "{\"TelegramPeer\":\"123\"}",
            "github",
            "vault:github_token",
            Some("ghp_"),
        )
        .expect("save");

        let all = j.load_all_pending_credential_prompts().expect("load");
        assert_eq!(all.len(), 2);
    }

    // ── Pending message deletion tests (credential-acquisition spec §8) ──

    #[test]
    fn test_pending_deletion_roundtrip() {
        let j = make_journal();
        j.save_pending_deletion("12345", "42").expect("save");

        let all = j.load_all_pending_deletions().expect("load");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "12345");
        assert_eq!(all[0].1, "42");

        j.delete_pending_deletion("12345", "42").expect("delete");
        let all2 = j.load_all_pending_deletions().expect("load");
        assert!(all2.is_empty());
    }

    #[test]
    fn test_pending_deletion_idempotent() {
        let j = make_journal();
        j.save_pending_deletion("100", "200").expect("save");
        // Saving again should not fail (INSERT OR REPLACE).
        j.save_pending_deletion("100", "200").expect("save again");

        let all = j.load_all_pending_deletions().expect("load");
        assert_eq!(all.len(), 1);
    }
}
