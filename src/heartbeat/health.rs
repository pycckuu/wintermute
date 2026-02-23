//! Health self-checks and `health.json` file writing.
//!
//! Gathers health data from all system components and writes an atomic
//! health report to disk each heartbeat tick.

use std::path::Path;
use std::time::Instant;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::HeartbeatDeps;

/// Health report written to `~/.wintermute/health.json` each heartbeat tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    /// Overall system status.
    pub status: String,
    /// Seconds since process start.
    pub uptime_secs: u64,
    /// ISO 8601 timestamp of this report.
    pub last_heartbeat: String,
    /// Executor type ("docker" or "direct").
    pub executor: String,
    /// Whether the container/executor is healthy.
    pub container_healthy: bool,
    /// Number of active user sessions.
    pub active_sessions: usize,
    /// Memory database size in megabytes.
    pub memory_db_size_mb: f64,
    /// Number of script files in /scripts/.
    pub scripts_count: usize,
    /// Number of registered dynamic tools.
    pub dynamic_tools_count: usize,
    /// Daily budget usage.
    pub budget_today: BudgetReport,
    /// Last error message, if any.
    pub last_error: Option<String>,
}

/// Budget usage snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetReport {
    /// Tokens used today.
    pub used: u64,
    /// Daily token limit.
    pub limit: u64,
}

/// Perform health self-checks and return a [`HealthReport`].
pub async fn check_health(deps: &HeartbeatDeps, start_time: Instant) -> HealthReport {
    let uptime_secs = start_time.elapsed().as_secs();
    let last_heartbeat = chrono::Utc::now().to_rfc3339();

    // Executor health.
    let (container_healthy, executor_kind, last_error) = match deps.executor.health_check().await {
        Ok(h) => (h.is_healthy(), format!("{:?}", deps.executor.kind()), None),
        Err(e) => (
            false,
            format!("{:?}", deps.executor.kind()),
            Some(e.to_string()),
        ),
    };

    // Memory database size.
    #[allow(clippy::cast_precision_loss)]
    let memory_db_size_mb = match deps.memory.db_size_bytes().await {
        Ok(bytes) => (bytes as f64) / (1024.0 * 1024.0),
        Err(e) => {
            warn!(error = %e, "failed to get memory db size");
            0.0
        }
    };

    // Count scripts (JSON files in scripts dir).
    let scripts_count = count_json_files(&deps.paths.scripts_dir).await;
    let dynamic_tools_count = scripts_count;

    // Active sessions.
    let active_sessions = deps.session_router.session_count().await;

    // Budget.
    let budget_used = deps.daily_budget.used();
    let budget_limit = deps.config.budget.max_tokens_per_day;

    let status = if container_healthy && last_error.is_none() {
        "running".to_owned()
    } else if container_healthy {
        "degraded".to_owned()
    } else {
        "unhealthy".to_owned()
    };

    HealthReport {
        status,
        uptime_secs,
        last_heartbeat,
        executor: executor_kind,
        container_healthy,
        active_sessions,
        memory_db_size_mb,
        scripts_count,
        dynamic_tools_count,
        budget_today: BudgetReport {
            used: budget_used,
            limit: budget_limit,
        },
        last_error,
    }
}

/// Write health report to disk atomically.
///
/// Writes to a temporary file first, then renames to the final path.
/// This ensures readers always see a complete file.
///
/// # Errors
///
/// Returns an error if serialization or file operations fail.
pub async fn write_health_file(report: &HealthReport, path: &Path) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(report).context("failed to serialize health report")?;

    let tmp_path = path.with_extension("json.tmp");
    tokio::fs::write(&tmp_path, json.as_bytes())
        .await
        .context("failed to write health temp file")?;

    tokio::fs::rename(&tmp_path, path)
        .await
        .context("failed to rename health temp file")?;

    debug!("health.json updated");
    Ok(())
}

/// Count `.json` files in a directory (non-recursive).
async fn count_json_files(dir: &Path) -> usize {
    let dir = dir.to_owned();
    tokio::task::spawn_blocking(move || count_json_files_sync(&dir))
        .await
        .unwrap_or(0)
}

/// Synchronous JSON file counter.
fn count_json_files_sync(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };

    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
        .count()
}
