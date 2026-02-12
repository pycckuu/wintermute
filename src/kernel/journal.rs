//! Task journal for crash recovery (feature spec: persistence-recovery, section 4).
//!
//! Persists task lifecycle state, pending approvals, pending credential prompts,
//! and adapter state to SQLite so the kernel can recover after restart.

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;
use uuid::Uuid;

use crate::types::SecurityLabel;

// ── Errors ──────────────────────────────────────────────────────

/// Journal operation errors (feature spec: persistence-recovery, section 4).
#[derive(Debug, Error)]
pub enum JournalError {
    /// SQLite database error.
    #[error("database error: {0}")]
    Database(String),
    /// Serialization/deserialization error.
    #[error("serialization error: {0}")]
    Serialization(String),
    /// Task not found in journal.
    #[error("task not found: {0}")]
    NotFound(Uuid),
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

/// Persisted task state — richer than the in-memory `TaskState` (feature spec: section 4.2).
///
/// Includes completed step details in `Executing`, approval IDs in
/// `AwaitingApproval`, and an `Abandoned` terminal state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistedTaskState {
    /// Phase 0 started but not completed.
    Extracting,
    /// Phase 0 done, Phase 1 (plan) started.
    Planning,
    /// Phase 1 done, executing steps.
    Executing {
        /// Next step to execute (0-based).
        current_step: usize,
        /// Steps that have been completed successfully.
        completed_steps: Vec<CompletedStep>,
    },
    /// All steps done, Phase 3 started.
    Synthesizing,
    /// Blocked on human approval.
    AwaitingApproval {
        /// ID of the pending approval request.
        approval_id: Uuid,
        /// Which plan step needs approval.
        step: usize,
    },
    /// Blocked on credential input from owner.
    AwaitingCredential {
        /// Service needing credentials (e.g. "notion", "github").
        service: String,
        /// Adapter message ID for reply matching.
        prompt_message_id: Option<String>,
    },
    /// Task completed successfully.
    Completed,
    /// Task failed.
    Failed,
    /// Task abandoned after restart (too old or unrecoverable).
    Abandoned,
}

/// A completed execution step persisted in the journal (feature spec: section 4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedStep {
    /// Step ordinal (1-based, matching plan).
    pub step: usize,
    /// Tool action ID (e.g. "email.list").
    pub tool: String,
    /// "read" or "write" — stored as string for SQLite simplicity.
    pub action_semantics: String,
    /// Structured result from the tool.
    pub result_json: serde_json::Value,
    /// Security label applied by kernel.
    pub label: SecurityLabel,
    /// When this step completed.
    pub completed_at: DateTime<Utc>,
}

/// Full persisted task row (feature spec: section 4.1).
#[derive(Debug, Clone)]
pub struct PersistedTask {
    /// Task UUID.
    pub task_id: Uuid,
    /// Template that created this task.
    pub template_id: String,
    /// JSON-serialized Principal.
    pub principal: String,
    /// JSON of the original trigger event (redacted).
    pub trigger_event: Option<String>,
    /// Current lifecycle state.
    pub state: PersistedTaskState,
    /// Serialized plan from Phase 1.
    pub plan_json: Option<String>,
    /// Serialized Phase 0 extracted metadata.
    pub extracted_metadata: Option<String>,
    /// Data ceiling from task template.
    pub data_ceiling: SecurityLabel,
    /// Output sinks as JSON array.
    pub output_sinks: Vec<String>,
    /// OpenTelemetry trace ID.
    pub trace_id: Option<String>,
    /// When the task was created.
    pub created_at: DateTime<Utc>,
    /// When the task was last updated.
    pub updated_at: DateTime<Utc>,
    /// Error message if failed.
    pub error: Option<String>,
}

/// Pending approval record surviving restart (feature spec: section 5.1).
#[derive(Debug, Clone)]
pub struct PendingApprovalRecord {
    /// Approval request UUID.
    pub approval_id: Uuid,
    /// Associated task.
    pub task_id: Uuid,
    /// Type of action needing approval.
    pub action_type: String,
    /// Human-readable description.
    pub description: String,
    /// Redacted data preview.
    pub data_preview: Option<String>,
    /// Taint level string ("Raw", "Extracted").
    pub taint_level: Option<String>,
    /// Target sink for the write.
    pub target_sink: Option<String>,
    /// Tool requesting the write.
    pub tool: Option<String>,
    /// Plan step number.
    pub step: Option<i64>,
    /// When created.
    pub created_at: DateTime<Utc>,
    /// When it expires.
    pub expires_at: DateTime<Utc>,
    /// Current status.
    pub status: String,
}

/// Pending credential prompt record surviving restart (feature spec: section 6.1).
#[derive(Debug, Clone)]
pub struct PendingCredentialRecord {
    /// Credential prompt UUID.
    pub prompt_id: Uuid,
    /// Associated task.
    pub task_id: Uuid,
    /// Service name (e.g. "notion").
    pub service: String,
    /// Credential type (e.g. "api_key", "oauth").
    pub credential_type: String,
    /// Setup instructions shown to owner.
    pub instructions: String,
    /// Vault reference for storage.
    pub vault_ref: String,
    /// Adapter message ID for reply matching.
    pub message_id: Option<String>,
    /// When created.
    pub created_at: DateTime<Utc>,
    /// When it expires.
    pub expires_at: DateTime<Utc>,
    /// Current status.
    pub status: String,
}

// ── State serialization helpers ─────────────────────────────────

/// Serialize `PersistedTaskState` to a (state_tag, execute_progress_json) pair
/// for storage in the `state` and `execute_progress` columns.
fn serialize_state(state: &PersistedTaskState) -> Result<(String, Option<String>), JournalError> {
    let tag = match state {
        PersistedTaskState::Extracting => "Extracting".to_owned(),
        PersistedTaskState::Planning => "Planning".to_owned(),
        PersistedTaskState::Executing { .. } => "Executing".to_owned(),
        PersistedTaskState::Synthesizing => "Synthesizing".to_owned(),
        PersistedTaskState::AwaitingApproval { approval_id, step } => {
            format!("AwaitingApproval:{}:{}", approval_id, step)
        }
        PersistedTaskState::AwaitingCredential {
            service,
            prompt_message_id,
        } => {
            let mid = prompt_message_id.as_deref().unwrap_or("");
            format!("AwaitingCredential:{}:{}", service, mid)
        }
        PersistedTaskState::Completed => "Completed".to_owned(),
        PersistedTaskState::Failed => "Failed".to_owned(),
        PersistedTaskState::Abandoned => "Abandoned".to_owned(),
    };

    let progress = match state {
        PersistedTaskState::Executing {
            current_step,
            completed_steps,
        } => {
            let steps_json: Vec<serde_json::Value> = completed_steps
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "step": s.step,
                        "tool": s.tool,
                        "action_semantics": s.action_semantics,
                        "result_json": s.result_json,
                        "label": s.label,
                        "completed_at": s.completed_at.to_rfc3339(),
                    })
                })
                .collect();
            Some(serde_json::to_string(&serde_json::json!({
                "current_step": current_step,
                "completed_steps": steps_json,
            }))?)
        }
        _ => None,
    };

    Ok((tag, progress))
}

/// Deserialize state from the `state` column tag and `execute_progress` JSON.
fn deserialize_state(
    tag: &str,
    execute_progress: Option<&str>,
) -> Result<PersistedTaskState, JournalError> {
    if tag == "Extracting" {
        return Ok(PersistedTaskState::Extracting);
    }
    if tag == "Planning" {
        return Ok(PersistedTaskState::Planning);
    }
    if tag == "Executing" {
        let progress = execute_progress.unwrap_or(r#"{"current_step":0,"completed_steps":[]}"#);
        let val: serde_json::Value = serde_json::from_str(progress)?;
        let current_step = usize::try_from(
            val.get("current_step")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        )
        .unwrap_or(0);
        let steps_arr = val
            .get("completed_steps")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut completed_steps = Vec::new();
        for s in &steps_arr {
            completed_steps.push(CompletedStep {
                step: usize::try_from(s.get("step").and_then(|v| v.as_u64()).unwrap_or(0))
                    .unwrap_or(0),
                tool: s
                    .get("tool")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned(),
                action_semantics: s
                    .get("action_semantics")
                    .and_then(|v| v.as_str())
                    .unwrap_or("read")
                    .to_owned(),
                result_json: s
                    .get("result_json")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                label: serde_json::from_value(
                    s.get("label")
                        .cloned()
                        .unwrap_or(serde_json::json!("public")),
                )
                .unwrap_or(SecurityLabel::Public),
                completed_at: s
                    .get("completed_at")
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now),
            });
        }
        return Ok(PersistedTaskState::Executing {
            current_step,
            completed_steps,
        });
    }
    if tag == "Synthesizing" {
        return Ok(PersistedTaskState::Synthesizing);
    }
    if let Some(rest) = tag.strip_prefix("AwaitingApproval:") {
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        let approval_id = parts
            .first()
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or(Uuid::nil());
        let step = parts
            .get(1)
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        return Ok(PersistedTaskState::AwaitingApproval { approval_id, step });
    }
    if let Some(rest) = tag.strip_prefix("AwaitingCredential:") {
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        let service = parts.first().unwrap_or(&"").to_string();
        let prompt_message_id = parts.get(1).and_then(|s| {
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        });
        return Ok(PersistedTaskState::AwaitingCredential {
            service,
            prompt_message_id,
        });
    }
    if tag == "Completed" {
        return Ok(PersistedTaskState::Completed);
    }
    if tag == "Failed" {
        return Ok(PersistedTaskState::Failed);
    }
    if tag == "Abandoned" {
        return Ok(PersistedTaskState::Abandoned);
    }
    Err(JournalError::Serialization(format!(
        "unknown state tag: {tag}"
    )))
}

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

// ── SQL Schema ──────────────────────────────────────────────────

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS task_journal (
    task_id         TEXT PRIMARY KEY,
    template_id     TEXT NOT NULL,
    principal       TEXT NOT NULL,
    trigger_event   TEXT,
    state           TEXT NOT NULL,
    phase           TEXT NOT NULL,
    plan_json       TEXT,
    execute_progress TEXT,
    extracted_metadata TEXT,
    data_ceiling    TEXT NOT NULL,
    output_sinks    TEXT NOT NULL,
    trace_id        TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    error           TEXT
);

CREATE INDEX IF NOT EXISTS idx_task_journal_state ON task_journal(state);
CREATE INDEX IF NOT EXISTS idx_task_journal_updated ON task_journal(updated_at);

CREATE TABLE IF NOT EXISTS pending_approvals (
    approval_id     TEXT PRIMARY KEY,
    task_id         TEXT NOT NULL,
    action_type     TEXT NOT NULL,
    description     TEXT NOT NULL,
    data_preview    TEXT,
    taint_level     TEXT,
    target_sink     TEXT,
    tool            TEXT,
    step            INTEGER,
    created_at      TEXT NOT NULL,
    expires_at      TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending'
);

CREATE INDEX IF NOT EXISTS idx_pending_approvals_status ON pending_approvals(status);

CREATE TABLE IF NOT EXISTS pending_credentials (
    prompt_id       TEXT PRIMARY KEY,
    task_id         TEXT NOT NULL,
    service         TEXT NOT NULL,
    credential_type TEXT NOT NULL,
    instructions    TEXT NOT NULL,
    vault_ref       TEXT NOT NULL,
    message_id      TEXT,
    created_at      TEXT NOT NULL,
    expires_at      TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending'
);

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
"#;

// ── TaskJournal ─────────────────────────────────────────────────

/// Parameters for creating a new task in the journal (feature spec: section 4.3).
#[derive(Debug, Clone)]
pub struct CreateTaskParams {
    /// Task UUID.
    pub task_id: Uuid,
    /// Template that created this task.
    pub template_id: String,
    /// JSON-serialized Principal.
    pub principal: String,
    /// JSON of the original trigger event (redacted).
    pub trigger_event: Option<String>,
    /// Data ceiling from task template.
    pub data_ceiling: SecurityLabel,
    /// Output sinks.
    pub output_sinks: Vec<String>,
    /// OpenTelemetry trace ID.
    pub trace_id: Option<String>,
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

/// SQLite-backed task journal for crash recovery (feature spec: section 4).
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
    /// Open a journal backed by a file (feature spec: section 4.1).
    pub fn open(path: &str) -> Result<Self, JournalError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory journal for testing (feature spec: section 4.1).
    pub fn open_in_memory() -> Result<Self, JournalError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // ── Task CRUD ───────────────────────────────────────────────

    /// Create a new task entry in the journal (feature spec: section 4.3).
    pub fn create_task(&self, params: &CreateTaskParams) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        let sinks_json = serde_json::to_string(&params.output_sinks)?;
        conn.execute(
            "INSERT INTO task_journal (task_id, template_id, principal, trigger_event, state, phase, data_ceiling, output_sinks, trace_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                params.task_id.to_string(),
                params.template_id,
                params.principal,
                params.trigger_event,
                "Extracting",
                "extract",
                label_to_str(params.data_ceiling),
                sinks_json,
                params.trace_id,
                now,
                now,
            ],
        )?;
        Ok(())
    }

    /// Update task state (feature spec: section 4.3).
    pub fn update_state(
        &self,
        task_id: Uuid,
        state: &PersistedTaskState,
    ) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let (tag, progress) = serialize_state(state)?;
        let phase = state_to_phase(state);
        let now = Utc::now().to_rfc3339();
        let rows = conn.execute(
            "UPDATE task_journal SET state = ?1, phase = ?2, execute_progress = ?3, updated_at = ?4 WHERE task_id = ?5",
            params![tag, phase, progress, now, task_id.to_string()],
        )?;
        if rows == 0 {
            return Err(JournalError::NotFound(task_id));
        }
        Ok(())
    }

    /// Store the plan JSON from Phase 1 (feature spec: section 4.3).
    pub fn update_plan(&self, task_id: Uuid, plan_json: &str) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        let rows = conn.execute(
            "UPDATE task_journal SET plan_json = ?1, updated_at = ?2 WHERE task_id = ?3",
            params![plan_json, now, task_id.to_string()],
        )?;
        if rows == 0 {
            return Err(JournalError::NotFound(task_id));
        }
        Ok(())
    }

    /// Store extracted metadata from Phase 0 (feature spec: section 4.3).
    pub fn update_extracted_metadata(
        &self,
        task_id: Uuid,
        metadata_json: &str,
    ) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        let rows = conn.execute(
            "UPDATE task_journal SET extracted_metadata = ?1, updated_at = ?2 WHERE task_id = ?3",
            params![metadata_json, now, task_id.to_string()],
        )?;
        if rows == 0 {
            return Err(JournalError::NotFound(task_id));
        }
        Ok(())
    }

    /// Append a completed step to the execute_progress (feature spec: section 4.3).
    pub fn append_completed_step(
        &self,
        task_id: Uuid,
        step: &CompletedStep,
    ) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let now = Utc::now().to_rfc3339();

        // Read current progress.
        let current: Option<String> = conn
            .query_row(
                "SELECT execute_progress FROM task_journal WHERE task_id = ?1",
                params![task_id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();

        let progress_str = current
            .as_deref()
            .unwrap_or(r#"{"current_step":0,"completed_steps":[]}"#);
        let mut progress: serde_json::Value = serde_json::from_str(progress_str)?;

        let step_json = serde_json::json!({
            "step": step.step,
            "tool": step.tool,
            "action_semantics": step.action_semantics,
            "result_json": step.result_json,
            "label": step.label,
            "completed_at": step.completed_at.to_rfc3339(),
        });

        if let Some(arr) = progress
            .get_mut("completed_steps")
            .and_then(|v| v.as_array_mut())
        {
            arr.push(step_json);
        }
        // Advance current_step.
        progress["current_step"] = serde_json::json!(step.step);

        let new_progress = serde_json::to_string(&progress)?;
        conn.execute(
            "UPDATE task_journal SET execute_progress = ?1, updated_at = ?2 WHERE task_id = ?3",
            params![new_progress, now, task_id.to_string()],
        )?;
        Ok(())
    }

    /// Mark task as completed (feature spec: section 4.3).
    pub fn mark_completed(&self, task_id: Uuid) -> Result<(), JournalError> {
        self.set_terminal_state(task_id, "Completed", None)
    }

    /// Mark task as failed with an error message (feature spec: section 4.3).
    pub fn mark_failed(&self, task_id: Uuid, error: &str) -> Result<(), JournalError> {
        self.set_terminal_state(task_id, "Failed", Some(error))
    }

    /// Mark task as abandoned (feature spec: section 7.2).
    pub fn mark_abandoned(&self, task_id: Uuid, reason: &str) -> Result<(), JournalError> {
        self.set_terminal_state(task_id, "Abandoned", Some(reason))
    }

    fn set_terminal_state(
        &self,
        task_id: Uuid,
        state: &str,
        error: Option<&str>,
    ) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        let rows = conn.execute(
            "UPDATE task_journal SET state = ?1, phase = ?2, error = ?3, updated_at = ?4 WHERE task_id = ?5",
            params![state, state.to_lowercase(), error, now, task_id.to_string()],
        )?;
        if rows == 0 {
            return Err(JournalError::NotFound(task_id));
        }
        Ok(())
    }

    /// Get a single task by ID (feature spec: section 4.1).
    pub fn get_task(&self, task_id: Uuid) -> Result<PersistedTask, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.query_row(
            "SELECT task_id, template_id, principal, trigger_event, state, plan_json, execute_progress, extracted_metadata, data_ceiling, output_sinks, trace_id, created_at, updated_at, error
             FROM task_journal WHERE task_id = ?1",
            params![task_id.to_string()],
            |row| Ok(row_to_persisted_task(row)),
        )?
        .map_err(|e| JournalError::Serialization(e.to_string()))
    }

    /// Load all non-terminal tasks for recovery (feature spec: section 7.2).
    pub fn load_incomplete_tasks(&self) -> Result<Vec<PersistedTask>, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT task_id, template_id, principal, trigger_event, state, plan_json, execute_progress, extracted_metadata, data_ceiling, output_sinks, trace_id, created_at, updated_at, error
             FROM task_journal WHERE state NOT IN ('Completed', 'Failed', 'Abandoned')",
        )?;
        let rows = stmt.query_map([], |row| Ok(row_to_persisted_task(row)))?;
        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?.map_err(|e| JournalError::Serialization(e.to_string()))?);
        }
        Ok(tasks)
    }

    /// Clean up old completed/failed/abandoned tasks (feature spec: section 4.4).
    ///
    /// Returns the number of deleted rows.
    pub fn cleanup_old_tasks(
        &self,
        completed_retention: std::time::Duration,
        failed_retention: std::time::Duration,
    ) -> Result<usize, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let now = Utc::now();
        let completed_cutoff = now
            .checked_sub_signed(chrono::Duration::seconds(
                i64::try_from(completed_retention.as_secs()).unwrap_or(i64::MAX),
            ))
            .unwrap_or(now)
            .to_rfc3339();
        let failed_cutoff = now
            .checked_sub_signed(chrono::Duration::seconds(
                i64::try_from(failed_retention.as_secs()).unwrap_or(i64::MAX),
            ))
            .unwrap_or(now)
            .to_rfc3339();

        let mut total = 0usize;
        total = total.saturating_add(conn.execute(
            "DELETE FROM task_journal WHERE state = 'Completed' AND updated_at < ?1",
            params![completed_cutoff],
        )?);
        total = total.saturating_add(conn.execute(
            "DELETE FROM task_journal WHERE state IN ('Failed', 'Abandoned') AND updated_at < ?1",
            params![failed_cutoff],
        )?);
        Ok(total)
    }

    // ── Pending approvals CRUD (feature spec: section 5) ────────

    /// Save a pending approval record.
    pub fn save_pending_approval(
        &self,
        record: &PendingApprovalRecord,
    ) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO pending_approvals (approval_id, task_id, action_type, description, data_preview, taint_level, target_sink, tool, step, created_at, expires_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                record.approval_id.to_string(),
                record.task_id.to_string(),
                record.action_type,
                record.description,
                record.data_preview,
                record.taint_level,
                record.target_sink,
                record.tool,
                record.step,
                record.created_at.to_rfc3339(),
                record.expires_at.to_rfc3339(),
                record.status,
            ],
        )?;
        Ok(())
    }

    /// Load a pending approval by ID.
    pub fn load_pending_approval(
        &self,
        approval_id: Uuid,
    ) -> Result<Option<PendingApprovalRecord>, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.query_row(
            "SELECT approval_id, task_id, action_type, description, data_preview, taint_level, target_sink, tool, step, created_at, expires_at, status
             FROM pending_approvals WHERE approval_id = ?1",
            params![approval_id.to_string()],
            |row| {
                Ok(PendingApprovalRecord {
                    approval_id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or(Uuid::nil()),
                    task_id: Uuid::parse_str(&row.get::<_, String>(1)?).unwrap_or(Uuid::nil()),
                    action_type: row.get(2)?,
                    description: row.get(3)?,
                    data_preview: row.get(4)?,
                    taint_level: row.get(5)?,
                    target_sink: row.get(6)?,
                    tool: row.get(7)?,
                    step: row.get(8)?,
                    created_at: parse_rfc3339_or_now(&row.get::<_, String>(9)?),
                    expires_at: parse_rfc3339_or_now(&row.get::<_, String>(10)?),
                    status: row.get(11)?,
                })
            },
        )
        .optional()
        .map_err(JournalError::from)
    }

    /// Load all pending approvals with a given status.
    pub fn load_pending_approvals_by_status(
        &self,
        status: &str,
    ) -> Result<Vec<PendingApprovalRecord>, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT approval_id, task_id, action_type, description, data_preview, taint_level, target_sink, tool, step, created_at, expires_at, status
             FROM pending_approvals WHERE status = ?1",
        )?;
        let rows = stmt.query_map(params![status], |row| {
            Ok(PendingApprovalRecord {
                approval_id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or(Uuid::nil()),
                task_id: Uuid::parse_str(&row.get::<_, String>(1)?).unwrap_or(Uuid::nil()),
                action_type: row.get(2)?,
                description: row.get(3)?,
                data_preview: row.get(4)?,
                taint_level: row.get(5)?,
                target_sink: row.get(6)?,
                tool: row.get(7)?,
                step: row.get(8)?,
                created_at: parse_rfc3339_or_now(&row.get::<_, String>(9)?),
                expires_at: parse_rfc3339_or_now(&row.get::<_, String>(10)?),
                status: row.get(11)?,
            })
        })?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    /// Update the status of a pending approval.
    pub fn update_approval_status(
        &self,
        approval_id: Uuid,
        status: &str,
    ) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.execute(
            "UPDATE pending_approvals SET status = ?1 WHERE approval_id = ?2",
            params![status, approval_id.to_string()],
        )?;
        Ok(())
    }

    // ── Pending credentials CRUD (feature spec: section 6) ──────

    /// Save a pending credential prompt record.
    pub fn save_pending_credential(
        &self,
        record: &PendingCredentialRecord,
    ) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO pending_credentials (prompt_id, task_id, service, credential_type, instructions, vault_ref, message_id, created_at, expires_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                record.prompt_id.to_string(),
                record.task_id.to_string(),
                record.service,
                record.credential_type,
                record.instructions,
                record.vault_ref,
                record.message_id,
                record.created_at.to_rfc3339(),
                record.expires_at.to_rfc3339(),
                record.status,
            ],
        )?;
        Ok(())
    }

    /// Load a pending credential prompt by ID.
    pub fn load_pending_credential(
        &self,
        prompt_id: Uuid,
    ) -> Result<Option<PendingCredentialRecord>, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.query_row(
            "SELECT prompt_id, task_id, service, credential_type, instructions, vault_ref, message_id, created_at, expires_at, status
             FROM pending_credentials WHERE prompt_id = ?1",
            params![prompt_id.to_string()],
            |row| {
                Ok(PendingCredentialRecord {
                    prompt_id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or(Uuid::nil()),
                    task_id: Uuid::parse_str(&row.get::<_, String>(1)?).unwrap_or(Uuid::nil()),
                    service: row.get(2)?,
                    credential_type: row.get(3)?,
                    instructions: row.get(4)?,
                    vault_ref: row.get(5)?,
                    message_id: row.get(6)?,
                    created_at: parse_rfc3339_or_now(&row.get::<_, String>(7)?),
                    expires_at: parse_rfc3339_or_now(&row.get::<_, String>(8)?),
                    status: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(JournalError::from)
    }

    /// Load all pending credential prompts with a given status.
    pub fn load_pending_credentials_by_status(
        &self,
        status: &str,
    ) -> Result<Vec<PendingCredentialRecord>, JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT prompt_id, task_id, service, credential_type, instructions, vault_ref, message_id, created_at, expires_at, status
             FROM pending_credentials WHERE status = ?1",
        )?;
        let rows = stmt.query_map(params![status], |row| {
            Ok(PendingCredentialRecord {
                prompt_id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or(Uuid::nil()),
                task_id: Uuid::parse_str(&row.get::<_, String>(1)?).unwrap_or(Uuid::nil()),
                service: row.get(2)?,
                credential_type: row.get(3)?,
                instructions: row.get(4)?,
                vault_ref: row.get(5)?,
                message_id: row.get(6)?,
                created_at: parse_rfc3339_or_now(&row.get::<_, String>(7)?),
                expires_at: parse_rfc3339_or_now(&row.get::<_, String>(8)?),
                status: row.get(9)?,
            })
        })?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    /// Update the status of a pending credential prompt.
    pub fn update_credential_status(
        &self,
        prompt_id: Uuid,
        status: &str,
    ) -> Result<(), JournalError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| JournalError::Database(e.to_string()))?;
        conn.execute(
            "UPDATE pending_credentials SET status = ?1 WHERE prompt_id = ?2",
            params![status, prompt_id.to_string()],
        )?;
        Ok(())
    }

    // ── Adapter state CRUD (feature spec: section 8.2) ──────────

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
}

// ── Helpers ─────────────────────────────────────────────────────

/// Map persisted state to phase string for the `phase` column.
fn state_to_phase(state: &PersistedTaskState) -> &'static str {
    match state {
        PersistedTaskState::Extracting => "extract",
        PersistedTaskState::Planning => "plan",
        PersistedTaskState::Executing { .. } => "execute",
        PersistedTaskState::Synthesizing => "synthesize",
        PersistedTaskState::AwaitingApproval { .. } => "execute",
        PersistedTaskState::AwaitingCredential { .. } => "execute",
        PersistedTaskState::Completed => "completed",
        PersistedTaskState::Failed => "failed",
        PersistedTaskState::Abandoned => "abandoned",
    }
}

/// Parse an RFC 3339 timestamp or return now.
fn parse_rfc3339_or_now(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

/// Convert a rusqlite Row to a PersistedTask.
fn row_to_persisted_task(row: &rusqlite::Row) -> Result<PersistedTask, JournalError> {
    let task_id_str: String = row
        .get(0)
        .map_err(|e| JournalError::Database(e.to_string()))?;
    let template_id: String = row
        .get(1)
        .map_err(|e| JournalError::Database(e.to_string()))?;
    let principal: String = row
        .get(2)
        .map_err(|e| JournalError::Database(e.to_string()))?;
    let trigger_event: Option<String> = row
        .get(3)
        .map_err(|e| JournalError::Database(e.to_string()))?;
    let state_tag: String = row
        .get(4)
        .map_err(|e| JournalError::Database(e.to_string()))?;
    let plan_json: Option<String> = row
        .get(5)
        .map_err(|e| JournalError::Database(e.to_string()))?;
    let execute_progress: Option<String> = row
        .get(6)
        .map_err(|e| JournalError::Database(e.to_string()))?;
    let extracted_metadata: Option<String> = row
        .get(7)
        .map_err(|e| JournalError::Database(e.to_string()))?;
    let data_ceiling_str: String = row
        .get(8)
        .map_err(|e| JournalError::Database(e.to_string()))?;
    let output_sinks_json: String = row
        .get(9)
        .map_err(|e| JournalError::Database(e.to_string()))?;
    let trace_id: Option<String> = row
        .get(10)
        .map_err(|e| JournalError::Database(e.to_string()))?;
    let created_at_str: String = row
        .get(11)
        .map_err(|e| JournalError::Database(e.to_string()))?;
    let updated_at_str: String = row
        .get(12)
        .map_err(|e| JournalError::Database(e.to_string()))?;
    let error: Option<String> = row
        .get(13)
        .map_err(|e| JournalError::Database(e.to_string()))?;

    let task_id =
        Uuid::parse_str(&task_id_str).map_err(|e| JournalError::Serialization(e.to_string()))?;
    let state = deserialize_state(&state_tag, execute_progress.as_deref())?;
    let data_ceiling = str_to_label(&data_ceiling_str);
    let output_sinks: Vec<String> = serde_json::from_str(&output_sinks_json)?;
    let created_at = parse_rfc3339_or_now(&created_at_str);
    let updated_at = parse_rfc3339_or_now(&updated_at_str);

    Ok(PersistedTask {
        task_id,
        template_id,
        principal,
        trigger_event,
        state,
        plan_json,
        extracted_metadata,
        data_ceiling,
        output_sinks,
        trace_id,
        created_at,
        updated_at,
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_journal() -> TaskJournal {
        TaskJournal::open_in_memory().expect("failed to create in-memory journal")
    }

    fn simple_params(task_id: Uuid) -> CreateTaskParams {
        CreateTaskParams {
            task_id,
            template_id: "tmpl".to_owned(),
            principal: "Owner".to_owned(),
            trigger_event: None,
            data_ceiling: SecurityLabel::Public,
            output_sinks: vec![],
            trace_id: None,
        }
    }

    #[test]
    fn test_create_and_get_task() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&CreateTaskParams {
            task_id: id,
            template_id: "owner_telegram_general".to_owned(),
            principal: "Owner".to_owned(),
            trigger_event: Some(r#"{"text":"hello"}"#.to_owned()),
            data_ceiling: SecurityLabel::Sensitive,
            output_sinks: vec!["sink:telegram:owner".to_owned()],
            trace_id: Some("trace-1".to_owned()),
        })
        .expect("create_task failed");

        let task = j.get_task(id).expect("get_task failed");
        assert_eq!(task.task_id, id);
        assert_eq!(task.template_id, "owner_telegram_general");
        assert_eq!(task.principal, "Owner");
        assert_eq!(task.state, PersistedTaskState::Extracting);
        assert_eq!(task.data_ceiling, SecurityLabel::Sensitive);
        assert_eq!(task.output_sinks, vec!["sink:telegram:owner"]);
    }

    #[test]
    fn test_get_task_not_found() {
        let j = make_journal();
        let result = j.get_task(Uuid::new_v4());
        assert!(result.is_err());
    }

    #[test]
    fn test_update_state_extracting_to_planning() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        j.update_state(id, &PersistedTaskState::Planning)
            .expect("update");
        let task = j.get_task(id).expect("get");
        assert_eq!(task.state, PersistedTaskState::Planning);
    }

    #[test]
    fn test_update_state_to_executing() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        let state = PersistedTaskState::Executing {
            current_step: 0,
            completed_steps: vec![],
        };
        j.update_state(id, &state).expect("update");
        let task = j.get_task(id).expect("get");
        assert_eq!(task.state, state);
    }

    #[test]
    fn test_update_state_awaiting_approval() {
        let j = make_journal();
        let id = Uuid::new_v4();
        let approval_id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        let state = PersistedTaskState::AwaitingApproval {
            approval_id,
            step: 2,
        };
        j.update_state(id, &state).expect("update");
        let task = j.get_task(id).expect("get");
        assert_eq!(task.state, state);
    }

    #[test]
    fn test_update_state_awaiting_credential() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        let state = PersistedTaskState::AwaitingCredential {
            service: "notion".to_owned(),
            prompt_message_id: Some("msg-123".to_owned()),
        };
        j.update_state(id, &state).expect("update");
        let task = j.get_task(id).expect("get");
        assert_eq!(task.state, state);
    }

    #[test]
    fn test_update_state_awaiting_credential_no_message_id() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        let state = PersistedTaskState::AwaitingCredential {
            service: "github".to_owned(),
            prompt_message_id: None,
        };
        j.update_state(id, &state).expect("update");
        let task = j.get_task(id).expect("get");
        assert_eq!(task.state, state);
    }

    #[test]
    fn test_update_plan() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        let plan = r#"{"plan":[{"step":1,"tool":"email.list","args":{}}]}"#;
        j.update_plan(id, plan).expect("update_plan");
        let task = j.get_task(id).expect("get");
        assert_eq!(task.plan_json.as_deref(), Some(plan));
    }

    #[test]
    fn test_update_extracted_metadata() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        let meta = r#"{"intent":"email_check"}"#;
        j.update_extracted_metadata(id, meta)
            .expect("update_metadata");
        let task = j.get_task(id).expect("get");
        assert_eq!(task.extracted_metadata.as_deref(), Some(meta));
    }

    #[test]
    fn test_append_completed_step() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        let state = PersistedTaskState::Executing {
            current_step: 0,
            completed_steps: vec![],
        };
        j.update_state(id, &state).expect("update");

        let step = CompletedStep {
            step: 1,
            tool: "email.list".to_owned(),
            action_semantics: "read".to_owned(),
            result_json: serde_json::json!({"emails": []}),
            label: SecurityLabel::Sensitive,
            completed_at: Utc::now(),
        };
        j.append_completed_step(id, &step).expect("append");

        let task = j.get_task(id).expect("get");
        if let PersistedTaskState::Executing {
            current_step,
            completed_steps,
        } = &task.state
        {
            assert_eq!(*current_step, 1);
            assert_eq!(completed_steps.len(), 1);
            assert_eq!(completed_steps[0].tool, "email.list");
            assert_eq!(completed_steps[0].action_semantics, "read");
            assert_eq!(completed_steps[0].label, SecurityLabel::Sensitive);
        } else {
            panic!("expected Executing state, got {:?}", task.state);
        }
    }

    #[test]
    fn test_append_multiple_steps() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        let state = PersistedTaskState::Executing {
            current_step: 0,
            completed_steps: vec![],
        };
        j.update_state(id, &state).expect("update");

        for i in 1..=3 {
            let step = CompletedStep {
                step: i,
                tool: format!("tool.step{i}"),
                action_semantics: "read".to_owned(),
                result_json: serde_json::json!({"step": i}),
                label: SecurityLabel::Public,
                completed_at: Utc::now(),
            };
            j.append_completed_step(id, &step).expect("append");
        }

        let task = j.get_task(id).expect("get");
        if let PersistedTaskState::Executing {
            completed_steps, ..
        } = &task.state
        {
            assert_eq!(completed_steps.len(), 3);
            assert_eq!(completed_steps[2].tool, "tool.step3");
        } else {
            panic!("expected Executing state");
        }
    }

    #[test]
    fn test_mark_completed() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        j.mark_completed(id).expect("mark_completed");
        let task = j.get_task(id).expect("get");
        assert_eq!(task.state, PersistedTaskState::Completed);
        assert!(task.error.is_none());
    }

    #[test]
    fn test_mark_failed() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        j.mark_failed(id, "test error").expect("mark_failed");
        let task = j.get_task(id).expect("get");
        assert_eq!(task.state, PersistedTaskState::Failed);
        assert_eq!(task.error.as_deref(), Some("test error"));
    }

    #[test]
    fn test_mark_abandoned() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        j.mark_abandoned(id, "too old").expect("mark_abandoned");
        let task = j.get_task(id).expect("get");
        assert_eq!(task.state, PersistedTaskState::Abandoned);
        assert_eq!(task.error.as_deref(), Some("too old"));
    }

    #[test]
    fn test_load_incomplete_tasks() {
        let j = make_journal();

        // Create 3 tasks in different states.
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let id3 = Uuid::new_v4();
        j.create_task(&simple_params(id1)).expect("create1");
        j.create_task(&simple_params(id2)).expect("create2");
        j.create_task(&simple_params(id3)).expect("create3");

        // id1 stays Extracting (incomplete).
        // id2 -> Completed (terminal).
        j.mark_completed(id2).expect("complete");
        // id3 -> Planning (incomplete).
        j.update_state(id3, &PersistedTaskState::Planning)
            .expect("update");

        let incomplete = j.load_incomplete_tasks().expect("load");
        let ids: Vec<Uuid> = incomplete.iter().map(|t| t.task_id).collect();
        assert!(ids.contains(&id1));
        assert!(!ids.contains(&id2));
        assert!(ids.contains(&id3));
    }

    #[test]
    fn test_cleanup_old_tasks() {
        let j = make_journal();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        j.create_task(&simple_params(id1)).expect("create1");
        j.create_task(&simple_params(id2)).expect("create2");
        j.mark_completed(id1).expect("complete");
        j.mark_failed(id2, "err").expect("fail");

        // Retention 0 => everything is old.
        let deleted = j
            .cleanup_old_tasks(Duration::from_secs(0), Duration::from_secs(0))
            .expect("cleanup");
        assert_eq!(deleted, 2);
    }

    #[test]
    fn test_cleanup_retains_recent() {
        let j = make_journal();
        let id1 = Uuid::new_v4();
        j.create_task(&simple_params(id1)).expect("create");
        j.mark_completed(id1).expect("complete");

        // Retention 1 hour => task was just completed, should be retained.
        let deleted = j
            .cleanup_old_tasks(Duration::from_secs(3600), Duration::from_secs(3600))
            .expect("cleanup");
        assert_eq!(deleted, 0);
    }

    #[test]
    fn test_update_state_not_found() {
        let j = make_journal();
        let result = j.update_state(Uuid::new_v4(), &PersistedTaskState::Planning);
        assert!(matches!(result, Err(JournalError::NotFound(_))));
    }

    #[test]
    fn test_all_security_labels_roundtrip() {
        let j = make_journal();
        for label in [
            SecurityLabel::Public,
            SecurityLabel::Internal,
            SecurityLabel::Sensitive,
            SecurityLabel::Regulated,
            SecurityLabel::Secret,
        ] {
            let id = Uuid::new_v4();
            let mut params = simple_params(id);
            params.data_ceiling = label;
            j.create_task(&params).expect("create");
            let task = j.get_task(id).expect("get");
            assert_eq!(
                task.data_ceiling, label,
                "label roundtrip failed for {label:?}"
            );
        }
    }

    // ── Pending approvals tests ─────────────────────────────────

    #[test]
    fn test_pending_approval_crud() {
        let j = make_journal();
        let record = PendingApprovalRecord {
            approval_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            action_type: "tainted_write".to_owned(),
            description: "Write to Notion".to_owned(),
            data_preview: Some("summary text".to_owned()),
            taint_level: Some("Raw".to_owned()),
            target_sink: Some("sink:notion:pages".to_owned()),
            tool: Some("notion.create_page".to_owned()),
            step: Some(2),
            created_at: Utc::now(),
            expires_at: Utc::now(),
            status: "pending".to_owned(),
        };
        j.save_pending_approval(&record).expect("save");

        let loaded = j
            .load_pending_approval(record.approval_id)
            .expect("load")
            .expect("should exist");
        assert_eq!(loaded.approval_id, record.approval_id);
        assert_eq!(loaded.description, "Write to Notion");
        assert_eq!(loaded.status, "pending");

        j.update_approval_status(record.approval_id, "approved")
            .expect("update");
        let updated = j
            .load_pending_approval(record.approval_id)
            .expect("load")
            .expect("should exist");
        assert_eq!(updated.status, "approved");
    }

    #[test]
    fn test_pending_approvals_by_status() {
        let j = make_journal();
        for i in 0..3 {
            let status = if i < 2 { "pending" } else { "approved" };
            let record = PendingApprovalRecord {
                approval_id: Uuid::new_v4(),
                task_id: Uuid::new_v4(),
                action_type: "tainted_write".to_owned(),
                description: format!("action {i}"),
                data_preview: None,
                taint_level: None,
                target_sink: None,
                tool: None,
                step: None,
                created_at: Utc::now(),
                expires_at: Utc::now(),
                status: status.to_owned(),
            };
            j.save_pending_approval(&record).expect("save");
        }
        let pending = j.load_pending_approvals_by_status("pending").expect("load");
        assert_eq!(pending.len(), 2);
    }

    // ── Pending credentials tests ───────────────────────────────

    #[test]
    fn test_pending_credential_crud() {
        let j = make_journal();
        let record = PendingCredentialRecord {
            prompt_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            service: "notion".to_owned(),
            credential_type: "integration_token".to_owned(),
            instructions: "Go to notion.so/my-integrations".to_owned(),
            vault_ref: "vault:notion_token".to_owned(),
            message_id: Some("tg-msg-123".to_owned()),
            created_at: Utc::now(),
            expires_at: Utc::now(),
            status: "pending".to_owned(),
        };
        j.save_pending_credential(&record).expect("save");

        let loaded = j
            .load_pending_credential(record.prompt_id)
            .expect("load")
            .expect("should exist");
        assert_eq!(loaded.service, "notion");
        assert_eq!(loaded.vault_ref, "vault:notion_token");

        j.update_credential_status(record.prompt_id, "received")
            .expect("update");
        let updated = j
            .load_pending_credential(record.prompt_id)
            .expect("load")
            .expect("should exist");
        assert_eq!(updated.status, "received");
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
