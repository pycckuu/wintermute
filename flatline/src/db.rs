//! Flatline state database backed by SQLite.
//!
//! Stores tool health statistics, fix history, and alert suppressions.
//! Migration is applied inline via `include_str!` on first open.

use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

/// Flatline's own SQLite state database.
pub struct StateDb {
    pool: SqlitePool,
}

/// A row from the `tool_stats` table representing an hourly bucket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStatRow {
    /// Name of the tool.
    pub tool_name: String,
    /// Hourly bucket start timestamp (ISO 8601 truncated to hour).
    pub window_start: String,
    /// Number of successful invocations in this bucket.
    pub success_count: i64,
    /// Number of failed invocations in this bucket.
    pub failure_count: i64,
    /// Average duration in milliseconds, if measured.
    pub avg_duration_ms: Option<i64>,
}

/// A row from the `updates` table tracking an update attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRecord {
    /// Auto-increment row ID.
    pub id: i64,
    /// When the update was checked (ISO 8601).
    pub checked_at: String,
    /// Version before the update.
    pub from_version: String,
    /// Target version.
    pub to_version: String,
    /// Current status of this update attempt.
    pub status: String,
    /// When the update application started (ISO 8601).
    pub started_at: Option<String>,
    /// When the update completed — success or failure (ISO 8601).
    pub completed_at: Option<String>,
    /// Reason for rollback, if any.
    pub rollback_reason: Option<String>,
    /// Captured stdout/stderr from migration scripts.
    pub migration_log: Option<String>,
}

/// A record tracking a proposed or applied fix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixRecord {
    /// Unique fix identifier (e.g. "fix-550e8400-e29b-...").
    pub id: String,
    /// When the issue was first detected (ISO 8601).
    pub detected_at: String,
    /// Pattern name that matched, if any.
    pub pattern: Option<String>,
    /// Human-readable diagnosis.
    pub diagnosis: Option<String>,
    /// Action taken or proposed.
    pub action: Option<String>,
    /// When the fix was applied (ISO 8601), if applicable.
    pub applied_at: Option<String>,
    /// Whether the fix was verified as effective.
    pub verified: Option<bool>,
    /// Whether the user was notified about this fix.
    pub user_notified: bool,
}

impl StateDb {
    /// Open (or create) the state database at the given path and apply migrations.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be opened or migration fails.
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create state db directory {}", parent.display())
            })?;
        }

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .pragma("trusted_schema", "OFF")
            .pragma("foreign_keys", "ON");

        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .with_context(|| format!("failed to open state db at {}", path.display()))?;

        let migration_sql = include_str!("../migrations/001_flatline_schema.sql");
        sqlx::raw_sql(migration_sql)
            .execute(&pool)
            .await
            .context("failed to apply flatline schema migration")?;

        Ok(Self { pool })
    }

    /// Record a tool execution statistic for an hourly bucket.
    ///
    /// Upserts into the `tool_stats` table: increments counts and recalculates
    /// the rolling average duration.
    ///
    /// # Errors
    ///
    /// Returns an error if the database write fails.
    pub async fn record_tool_stat(
        &self,
        tool_name: &str,
        window_start: &str,
        success: bool,
        duration_ms: Option<i64>,
    ) -> anyhow::Result<()> {
        let success_inc: i64 = if success { 1 } else { 0 };
        let failure_inc: i64 = if success { 0 } else { 1 };

        // Upsert: insert or update on conflict.
        // For avg_duration_ms we use a simple running average:
        // new_avg = old_avg + (new_value - old_avg) / new_count
        // When duration_ms is NULL we keep the existing average.
        sqlx::query(
            r"INSERT INTO tool_stats (tool_name, window_start, success_count, failure_count, avg_duration_ms)
              VALUES (?1, ?2, ?3, ?4, ?5)
              ON CONFLICT(tool_name, window_start) DO UPDATE SET
                success_count = success_count + ?3,
                failure_count = failure_count + ?4,
                avg_duration_ms = CASE
                    WHEN ?5 IS NOT NULL AND avg_duration_ms IS NOT NULL
                        THEN (avg_duration_ms * (success_count + failure_count - ?3 - ?4) + ?5) /
                             (success_count + failure_count - ?3 - ?4 + 1)
                    WHEN ?5 IS NOT NULL THEN ?5
                    ELSE avg_duration_ms
                END",
        )
        .bind(tool_name)
        .bind(window_start)
        .bind(success_inc)
        .bind(failure_inc)
        .bind(duration_ms)
        .execute(&self.pool)
        .await
        .context("failed to record tool stat")?;

        Ok(())
    }

    /// Query tool statistics for a given tool since a point in time.
    ///
    /// # Errors
    ///
    /// Returns an error if the database read fails.
    pub async fn tool_stats(
        &self,
        tool_name: &str,
        since: &str,
    ) -> anyhow::Result<Vec<ToolStatRow>> {
        let rows: Vec<ToolStatRow> = sqlx::query_as::<_, (String, String, i64, i64, Option<i64>)>(
            "SELECT tool_name, window_start, success_count, failure_count, avg_duration_ms
             FROM tool_stats
             WHERE tool_name = ?1 AND window_start >= ?2
             ORDER BY window_start ASC",
        )
        .bind(tool_name)
        .bind(since)
        .fetch_all(&self.pool)
        .await
        .context("failed to query tool stats")?
        .into_iter()
        .map(
            |(tool_name, window_start, success_count, failure_count, avg_duration_ms)| {
                ToolStatRow {
                    tool_name,
                    window_start,
                    success_count,
                    failure_count,
                    avg_duration_ms,
                }
            },
        )
        .collect();

        Ok(rows)
    }

    /// Insert a new fix record.
    ///
    /// # Errors
    ///
    /// Returns an error if the database write fails.
    pub async fn insert_fix(&self, fix: &FixRecord) -> anyhow::Result<()> {
        let verified_int: Option<i64> = fix.verified.map(|v| if v { 1 } else { 0 });
        let notified_int: i64 = if fix.user_notified { 1 } else { 0 };

        sqlx::query(
            "INSERT INTO fixes (id, detected_at, pattern, diagnosis, action, applied_at, verified, user_notified)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(&fix.id)
        .bind(&fix.detected_at)
        .bind(&fix.pattern)
        .bind(&fix.diagnosis)
        .bind(&fix.action)
        .bind(&fix.applied_at)
        .bind(verified_int)
        .bind(notified_int)
        .execute(&self.pool)
        .await
        .context("failed to insert fix record")?;

        Ok(())
    }

    /// Update status fields on an existing fix record.
    ///
    /// # Errors
    ///
    /// Returns an error if the database write fails.
    pub async fn update_fix(
        &self,
        id: &str,
        applied_at: Option<&str>,
        verified: Option<bool>,
        user_notified: Option<bool>,
    ) -> anyhow::Result<()> {
        let verified_int: Option<i64> = verified.map(|v| if v { 1 } else { 0 });
        let notified_int: Option<i64> = user_notified.map(|v| if v { 1 } else { 0 });

        sqlx::query(
            "UPDATE fixes SET
                applied_at = COALESCE(?2, applied_at),
                verified = COALESCE(?3, verified),
                user_notified = COALESCE(?4, user_notified)
             WHERE id = ?1",
        )
        .bind(id)
        .bind(applied_at)
        .bind(verified_int)
        .bind(notified_int)
        .execute(&self.pool)
        .await
        .context("failed to update fix record")?;

        Ok(())
    }

    /// Query the most recent fix records.
    ///
    /// # Errors
    ///
    /// Returns an error if the database read fails.
    pub async fn recent_fixes(&self, limit: i64) -> anyhow::Result<Vec<FixRecord>> {
        let rows = sqlx::query_as::<_, FixRow>(
            "SELECT id, detected_at, pattern, diagnosis, action, applied_at, verified, user_notified
             FROM fixes
             ORDER BY detected_at DESC
             LIMIT ?1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to query recent fixes")?;

        let fixes = rows.into_iter().map(fix_row_into_record).collect();
        Ok(fixes)
    }

    /// Check whether alerts for a pattern are currently suppressed.
    ///
    /// A pattern is suppressed if it exists in the suppressions table and
    /// either has no expiry or the expiry is in the future.
    ///
    /// # Errors
    ///
    /// Returns an error if the database read fails.
    pub async fn is_suppressed(&self, pattern: &str) -> anyhow::Result<bool> {
        let now = chrono::Utc::now().to_rfc3339();
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT pattern FROM suppressions
             WHERE pattern = ?1
               AND (suppressed_until IS NULL OR suppressed_until > ?2)",
        )
        .bind(pattern)
        .bind(&now)
        .fetch_optional(&self.pool)
        .await
        .context("failed to check suppression")?;

        Ok(row.is_some())
    }

    /// List distinct tool names that have statistics since the given timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error if the database read fails.
    pub async fn distinct_tool_names(&self, since: &str) -> anyhow::Result<Vec<String>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT DISTINCT tool_name FROM tool_stats WHERE window_start >= ?1")
                .bind(since)
                .fetch_all(&self.pool)
                .await
                .context("failed to query distinct tool names")?;

        Ok(rows.into_iter().map(|(name,)| name).collect())
    }

    /// Suppress alerts for a pattern until a given time.
    ///
    /// # Errors
    ///
    /// Returns an error if the database write fails.
    pub async fn suppress(
        &self,
        pattern: &str,
        until: Option<&str>,
        reason: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO suppressions (pattern, suppressed_until, reason)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(pattern) DO UPDATE SET
                suppressed_until = ?2,
                reason = ?3",
        )
        .bind(pattern)
        .bind(until)
        .bind(reason)
        .execute(&self.pool)
        .await
        .context("failed to add suppression")?;

        Ok(())
    }

    // -- Update tracking methods --

    /// Insert a new update record. Returns the assigned row ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the database write fails.
    pub async fn insert_update(&self, record: &UpdateRecord) -> anyhow::Result<i64> {
        let result = sqlx::query(
            "INSERT INTO updates (checked_at, from_version, to_version, status, started_at, completed_at, rollback_reason, migration_log)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(&record.checked_at)
        .bind(&record.from_version)
        .bind(&record.to_version)
        .bind(&record.status)
        .bind(&record.started_at)
        .bind(&record.completed_at)
        .bind(&record.rollback_reason)
        .bind(&record.migration_log)
        .execute(&self.pool)
        .await
        .context("failed to insert update record")?;

        Ok(result.last_insert_rowid())
    }

    /// Update the status and optional fields of an existing update record.
    ///
    /// # Errors
    ///
    /// Returns an error if the database write fails.
    pub async fn set_update_status(
        &self,
        id: i64,
        status: &str,
        started_at: Option<&str>,
        completed_at: Option<&str>,
        rollback_reason: Option<&str>,
        migration_log: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE updates SET
                status = ?2,
                started_at = COALESCE(?3, started_at),
                completed_at = COALESCE(?4, completed_at),
                rollback_reason = COALESCE(?5, rollback_reason),
                migration_log = COALESCE(?6, migration_log)
             WHERE id = ?1",
        )
        .bind(id)
        .bind(status)
        .bind(started_at)
        .bind(completed_at)
        .bind(rollback_reason)
        .bind(migration_log)
        .execute(&self.pool)
        .await
        .context("failed to update update record")?;

        Ok(())
    }

    /// Get the most recent update record.
    ///
    /// # Errors
    ///
    /// Returns an error if the database read fails.
    pub async fn latest_update(&self) -> anyhow::Result<Option<UpdateRecord>> {
        let row: Option<UpdateRow> = sqlx::query_as(
            "SELECT id, checked_at, from_version, to_version, status, started_at, completed_at, rollback_reason, migration_log
             FROM updates
             ORDER BY id DESC
             LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .context("failed to query latest update")?;

        Ok(row.map(update_row_into_record))
    }

    /// Get any pending update (status = 'pending' or 'downloading').
    ///
    /// # Errors
    ///
    /// Returns an error if the database read fails.
    pub async fn pending_update(&self) -> anyhow::Result<Option<UpdateRecord>> {
        let row: Option<UpdateRow> = sqlx::query_as(
            "SELECT id, checked_at, from_version, to_version, status, started_at, completed_at, rollback_reason, migration_log
             FROM updates
             WHERE status IN ('pending', 'downloading')
             ORDER BY id DESC
             LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .context("failed to query pending update")?;

        Ok(row.map(update_row_into_record))
    }
}

/// Raw row tuple from the `updates` table.
type UpdateRow = (
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Convert a raw `updates` row tuple into an [`UpdateRecord`].
fn update_row_into_record(row: UpdateRow) -> UpdateRecord {
    let (
        id,
        checked_at,
        from_version,
        to_version,
        status,
        started_at,
        completed_at,
        rollback_reason,
        migration_log,
    ) = row;
    UpdateRecord {
        id,
        checked_at,
        from_version,
        to_version,
        status,
        started_at,
        completed_at,
        rollback_reason,
        migration_log,
    }
}

/// Raw row tuple from the `fixes` table, used to avoid an 8-element inline
/// tuple type in `recent_fixes`.
type FixRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    i64,
);

/// Convert a raw `fixes` row tuple into a [`FixRecord`].
fn fix_row_into_record(row: FixRow) -> FixRecord {
    let (id, detected_at, pattern, diagnosis, action, applied_at, verified, user_notified) = row;
    FixRecord {
        id,
        detected_at,
        pattern,
        diagnosis,
        action,
        applied_at,
        verified: verified.map(|v| v != 0),
        user_notified: user_notified != 0,
    }
}
