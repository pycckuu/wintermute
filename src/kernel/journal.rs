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
}
