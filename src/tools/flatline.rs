//! Read-only access to Flatline supervisor state and logs.
//!
//! Provides the `flatline_status` tool which queries Flatline's SQLite
//! state database and reads its structured JSONL log files. The tool
//! opens a short-lived read-only connection per call — no persistent pool.

use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use sqlx::sqlite::SqliteRow;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use tracing::debug;

use crate::providers::ToolDefinition;

use super::ToolError;

// ---------------------------------------------------------------------------
// Serializable response types
// ---------------------------------------------------------------------------

/// Summary combining latest update, recent fixes, and active suppressions.
#[derive(Debug, Serialize)]
struct SummaryResponse {
    /// The most recent update record, if any.
    latest_update: Option<UpdateRow>,
    /// The 5 most recent fix records.
    recent_fixes: Vec<FixRow>,
    /// Currently active suppressions.
    active_suppressions: Vec<SuppressionRow>,
}

/// A row from the `updates` table.
#[derive(Debug, Serialize)]
struct UpdateRow {
    /// When the update check occurred.
    checked_at: String,
    /// Version before the update.
    from_version: String,
    /// Version being updated to.
    to_version: String,
    /// Update lifecycle status.
    status: String,
    /// When the update application started.
    started_at: Option<String>,
    /// When the update completed.
    completed_at: Option<String>,
    /// Reason for rollback, if applicable.
    rollback_reason: Option<String>,
}

impl UpdateRow {
    /// Build from a SQLite row with the expected column names.
    fn from_row(row: &SqliteRow) -> Self {
        Self {
            checked_at: row.get("checked_at"),
            from_version: row.get("from_version"),
            to_version: row.get("to_version"),
            status: row.get("status"),
            started_at: row.get("started_at"),
            completed_at: row.get("completed_at"),
            rollback_reason: row.get("rollback_reason"),
        }
    }
}

/// A row from the `fixes` table.
#[derive(Debug, Serialize)]
struct FixRow {
    /// Unique fix identifier.
    id: String,
    /// When the issue was detected.
    detected_at: String,
    /// Pattern that triggered the fix.
    pattern: Option<String>,
    /// Action taken to fix the issue.
    action: Option<String>,
    /// When the fix was applied.
    applied_at: Option<String>,
    /// Whether the fix was verified.
    verified: Option<bool>,
}

impl FixRow {
    /// Build from a SQLite row with the expected column names.
    fn from_row(row: &SqliteRow) -> Self {
        Self {
            id: row.get("id"),
            detected_at: row.get("detected_at"),
            pattern: row.get("pattern"),
            action: row.get("action"),
            applied_at: row.get("applied_at"),
            verified: row.get::<Option<i32>, _>("verified").map(|v| v != 0),
        }
    }
}

/// Aggregated tool statistics for the last 24 hours.
#[derive(Debug, Serialize)]
struct ToolStatRow {
    /// Name of the tool.
    tool_name: String,
    /// Total successful invocations.
    success_count: i64,
    /// Total failed invocations.
    failure_count: i64,
    /// Average execution duration in milliseconds.
    avg_duration_ms: Option<i64>,
}

/// A row from the `suppressions` table.
#[derive(Debug, Serialize)]
struct SuppressionRow {
    /// Pattern being suppressed.
    pattern: String,
    /// When the suppression expires (None = indefinite).
    suppressed_until: Option<String>,
    /// Reason for the suppression.
    reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Open a read-only single-connection pool to Flatline's `state.db`.
async fn open_readonly(db_path: &Path) -> Result<SqlitePool, ToolError> {
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .read_only(true)
        .pragma("trusted_schema", "OFF");

    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to open flatline state.db: {e}")))
}

/// Open `state.db` under `flatline_root`, checking existence first.
///
/// Returns a read-only pool that the caller must close after querying.
async fn open_state_db(flatline_root: &Path) -> Result<SqlitePool, ToolError> {
    let db_path = flatline_root.join("state.db");
    if !db_path.exists() {
        return Err(ToolError::ExecutionFailed(
            "flatline state.db not found — supervisor may not have run yet".to_owned(),
        ));
    }
    open_readonly(&db_path).await
}

/// Serialize a value to pretty JSON, mapping errors to `ToolError`.
fn to_json<T: Serialize>(value: &T) -> Result<String, ToolError> {
    serde_json::to_string_pretty(value)
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to serialize response: {e}")))
}

/// Find the latest log file in a directory by modification time.
///
/// Matches files with `.jsonl` extension or names containing `.log`.
fn find_latest_log(logs_dir: &Path) -> Result<Option<PathBuf>, ToolError> {
    if !logs_dir.exists() {
        return Ok(None);
    }

    let entries = fs::read_dir(logs_dir)
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to read logs dir: {e}")))?;

    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;

    for entry in entries {
        let entry = entry.map_err(|e| {
            ToolError::ExecutionFailed(format!("failed to read log dir entry: {e}"))
        })?;
        let path = entry.path();

        // Only consider regular files (skip symlinks for defence-in-depth).
        let file_type = entry
            .file_type()
            .map_err(|e| ToolError::ExecutionFailed(format!("failed to read file type: {e}")))?;
        if !file_type.is_file() {
            continue;
        }

        let is_log = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".jsonl") || name.contains(".log"));

        if !is_log {
            continue;
        }

        let modified = entry.metadata().and_then(|m| m.modified()).map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "failed to read metadata for {}: {e}",
                path.display()
            ))
        })?;

        let is_newer = best
            .as_ref()
            .is_none_or(|(_, best_time)| modified > *best_time);

        if is_newer {
            best = Some((path, modified));
        }
    }

    Ok(best.map(|(path, _)| path))
}

/// Read the last `limit` lines from a file.
///
/// Uses a seek-from-end approach to avoid reading the entire file.
/// Caps total bytes read to prevent memory exhaustion from large files.
fn read_tail_lines(path: &Path, limit: usize) -> Result<String, ToolError> {
    /// Maximum bytes to read from a log file (1 MB safety cap).
    const MAX_READ_BYTES: usize = 1024 * 1024;

    let file = fs::File::open(path)
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to open log file: {e}")))?;

    let metadata = file.metadata().map_err(|e| {
        ToolError::ExecutionFailed(format!("failed to read log file metadata: {e}"))
    })?;

    let file_len = metadata.len();
    if file_len == 0 {
        return Ok(String::new());
    }

    // For small files, read the whole thing.
    const SMALL_FILE_THRESHOLD: u64 = 64 * 1024;
    if file_len <= SMALL_FILE_THRESHOLD {
        let mut reader = BufReader::new(file);
        let mut contents = String::new();
        reader
            .read_to_string(&mut contents)
            .map_err(|e| ToolError::ExecutionFailed(format!("failed to read log file: {e}")))?;

        let lines: Vec<&str> = contents.lines().collect();
        let start = lines.len().saturating_sub(limit);
        return Ok(lines[start..].join("\n"));
    }

    // For large files, seek from end and read a bounded chunk.
    let chunk_size = (limit as u64)
        .saturating_mul(200)
        .min(file_len)
        .min(MAX_READ_BYTES as u64);
    let seek_pos = file_len.saturating_sub(chunk_size);

    let mut reader = BufReader::new(file);
    reader
        .seek(SeekFrom::Start(seek_pos))
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to seek in log file: {e}")))?;

    // If we didn't seek to the start, skip the first partial line.
    if seek_pos > 0 {
        let mut discard = String::new();
        let _ = reader.read_line(&mut discard);
    }

    let mut lines = Vec::new();
    let mut bytes_read: usize = 0;
    for line_result in reader.lines() {
        let line = line_result
            .map_err(|e| ToolError::ExecutionFailed(format!("failed to read log line: {e}")))?;
        bytes_read = bytes_read.saturating_add(line.len());
        if bytes_read > MAX_READ_BYTES {
            break;
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }

    let start = lines.len().saturating_sub(limit);
    Ok(lines[start..].join("\n"))
}

// ---------------------------------------------------------------------------
// Query functions
// ---------------------------------------------------------------------------

/// Query a summary: latest update, recent fixes, active suppressions.
async fn query_summary(pool: &SqlitePool) -> Result<String, ToolError> {
    let latest_update = sqlx::query(
        "SELECT checked_at, from_version, to_version, status, \
         started_at, completed_at, rollback_reason \
         FROM updates ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| ToolError::ExecutionFailed(format!("failed to query updates: {e}")))?
    .as_ref()
    .map(UpdateRow::from_row);

    let recent_fixes: Vec<FixRow> = sqlx::query(
        "SELECT id, detected_at, pattern, action, applied_at, verified \
         FROM fixes ORDER BY detected_at DESC LIMIT 5",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ToolError::ExecutionFailed(format!("failed to query fixes: {e}")))?
    .iter()
    .map(FixRow::from_row)
    .collect();

    let active_suppressions: Vec<SuppressionRow> = sqlx::query(
        "SELECT pattern, suppressed_until, reason FROM suppressions \
         WHERE suppressed_until IS NULL OR suppressed_until > datetime('now')",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ToolError::ExecutionFailed(format!("failed to query suppressions: {e}")))?
    .into_iter()
    .map(|row| SuppressionRow {
        pattern: row.get("pattern"),
        suppressed_until: row.get("suppressed_until"),
        reason: row.get("reason"),
    })
    .collect();

    to_json(&SummaryResponse {
        latest_update,
        recent_fixes,
        active_suppressions,
    })
}

/// Query the last 10 updates.
async fn query_updates(pool: &SqlitePool) -> Result<String, ToolError> {
    let updates: Vec<UpdateRow> = sqlx::query(
        "SELECT checked_at, from_version, to_version, status, \
         started_at, completed_at, rollback_reason \
         FROM updates ORDER BY id DESC LIMIT 10",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ToolError::ExecutionFailed(format!("failed to query updates: {e}")))?
    .iter()
    .map(UpdateRow::from_row)
    .collect();

    to_json(&updates)
}

/// Query the last 10 fixes.
async fn query_fixes(pool: &SqlitePool) -> Result<String, ToolError> {
    let fixes: Vec<FixRow> = sqlx::query(
        "SELECT id, detected_at, pattern, action, applied_at, verified \
         FROM fixes ORDER BY detected_at DESC LIMIT 10",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ToolError::ExecutionFailed(format!("failed to query fixes: {e}")))?
    .iter()
    .map(FixRow::from_row)
    .collect();

    to_json(&fixes)
}

/// Query tool stats from the last 24 hours, aggregated by tool name.
async fn query_stats(pool: &SqlitePool) -> Result<String, ToolError> {
    let stats: Vec<ToolStatRow> = sqlx::query(
        "SELECT tool_name, \
         SUM(success_count) AS success_count, \
         SUM(failure_count) AS failure_count, \
         AVG(avg_duration_ms) AS avg_duration_ms \
         FROM tool_stats \
         WHERE window_start >= datetime('now', '-24 hours') \
         GROUP BY tool_name \
         ORDER BY SUM(failure_count) DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ToolError::ExecutionFailed(format!("failed to query tool_stats: {e}")))?
    .into_iter()
    .map(|row| ToolStatRow {
        tool_name: row.get("tool_name"),
        success_count: row.get("success_count"),
        failure_count: row.get("failure_count"),
        avg_duration_ms: row.get("avg_duration_ms"),
    })
    .collect();

    to_json(&stats)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Maximum number of log lines to return (safety cap).
const MAX_LOG_LINES: usize = 200;

/// Default number of log lines when `limit` is not specified.
const DEFAULT_LOG_LINES: usize = 50;

/// Query Flatline supervisor state or activity logs.
///
/// Reads Flatline's SQLite state database and/or JSONL log files.
/// All reads are read-only — this tool never modifies Flatline's state.
///
/// # Sections
///
/// - (none): Summary with latest update, recent fixes, active suppressions
/// - `"updates"`: Last 10 update records
/// - `"fixes"`: Last 10 fix records
/// - `"stats"`: Tool failure statistics from the last 24 hours
/// - `"logs"`: Recent Flatline activity log lines (JSONL)
///
/// # Errors
///
/// Returns `ToolError::ExecutionFailed` if Flatline is not installed,
/// the database cannot be opened, or log files cannot be read.
pub async fn flatline_status(flatline_root: &Path, input: &Value) -> Result<String, ToolError> {
    if !flatline_root.exists() {
        return Err(ToolError::ExecutionFailed(
            "flatline supervisor not installed (directory not found)".to_owned(),
        ));
    }

    let section = input.get("section").and_then(|v| v.as_str());

    match section {
        None | Some("updates") | Some("fixes") | Some("stats") => {
            let pool = open_state_db(flatline_root).await?;
            let result = match section {
                None => query_summary(&pool).await,
                Some("updates") => query_updates(&pool).await,
                Some("fixes") => query_fixes(&pool).await,
                Some("stats") => query_stats(&pool).await,
                _ => unreachable!(),
            };
            pool.close().await;
            result
        }
        Some("logs") => {
            let logs_dir = flatline_root.join("logs");
            let limit = input
                .get("limit")
                .and_then(|v| v.as_u64())
                .and_then(|v| usize::try_from(v).ok())
                .map(|v| v.min(MAX_LOG_LINES))
                .unwrap_or(DEFAULT_LOG_LINES);

            debug!(limit, "reading flatline logs");

            match find_latest_log(&logs_dir)? {
                Some(log_path) => read_tail_lines(&log_path, limit),
                None => Ok("No Flatline log files found.".to_owned()),
            }
        }
        Some(other) => Err(ToolError::InvalidInput(format!(
            "unknown section: {other}; valid: updates, fixes, stats, logs"
        ))),
    }
}

/// Return the tool definition for `flatline_status`.
pub fn flatline_status_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "flatline_status".to_owned(),
        description: "Query Flatline supervisor: update history, applied fixes, tool stats, \
                       and activity logs. Omit 'section' for a summary, or use 'logs' to read \
                       recent Flatline log entries."
            .to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "section": {
                    "type": "string",
                    "enum": ["updates", "fixes", "stats", "logs"],
                    "description": "Which section to query. Omit for a summary of all DB sections."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max log lines to return (default 50, only for 'logs' section)."
                }
            }
        }),
    }
}
