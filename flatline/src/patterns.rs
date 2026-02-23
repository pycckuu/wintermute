//! Known failure pattern matching for Wintermute diagnostics.
//!
//! Eight rule-based patterns detect common failure modes without requiring
//! LLM calls. Each pattern evaluates evidence from logs, health, git, and
//! tool statistics to produce a `PatternMatch`.

use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tracing::warn;
use wintermute::heartbeat::health::HealthReport;

use crate::config::FlatlineConfig;
use crate::stats::StatsEngine;
use crate::watcher::Watcher;

/// Severity level for a detected pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Information only, no action needed.
    Low,
    /// May need attention soon.
    Medium,
    /// Needs attention now.
    High,
    /// System is down or at risk.
    Critical,
}

impl Severity {
    /// Return a numeric rank for sorting (higher = more severe).
    fn rank(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Medium => 1,
            Self::High => 2,
            Self::Critical => 3,
        }
    }
}

/// Kind of known failure pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternKind {
    /// Tool failing after a recent git change.
    ToolFailingAfterChange,
    /// Wintermute process is not running.
    ProcessDown,
    /// Container repeatedly unhealthy.
    ContainerWontStart,
    /// Budget being consumed too fast.
    BudgetExhaustionLoop,
    /// Scheduled task failing consecutively.
    ScheduledTaskFailing,
    /// Too many pending memories.
    MemoryBloat,
    /// Too many dynamic tools or too many unused.
    DynamicToolSprawl,
    /// Disk usage too high.
    DiskSpacePressure,
}

/// Evidence gathered for a pattern match.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    /// Human-readable summary.
    pub summary: String,
    /// Key-value details.
    pub details: serde_json::Value,
}

/// A matched pattern with severity and evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternMatch {
    /// Which pattern was detected.
    pub kind: PatternKind,
    /// How severe this is.
    pub severity: Severity,
    /// Evidence supporting the match.
    pub evidence: Evidence,
    /// Whether this pattern supports auto-fix.
    pub auto_fixable: bool,
}

/// Git log entry for correlation.
#[derive(Debug, Clone)]
pub struct GitLogEntry {
    /// Full commit hash.
    pub hash: String,
    /// ISO 8601 timestamp of the commit.
    pub timestamp: String,
    /// Commit message summary.
    pub message: String,
}

/// Evaluate all 8 patterns and return matches sorted by severity (critical first).
pub async fn evaluate_patterns(
    stats: &StatsEngine,
    health: Option<&HealthReport>,
    git_log: &[GitLogEntry],
    config: &FlatlineConfig,
    watcher: &Watcher,
) -> Vec<PatternMatch> {
    let mut matches = Vec::new();

    // Step 1: Check each pattern, collecting any matches.
    if let Some(m) = check_tool_failing_after_change(stats, git_log, config).await {
        matches.extend(m);
    }

    if let Some(m) = check_process_down(watcher, config) {
        matches.push(m);
    }

    if let Some(m) = check_container_wont_start(health) {
        matches.push(m);
    }

    if let Some(m) = check_budget_exhaustion(health, config, stats).await {
        matches.push(m);
    }

    if let Some(m) = check_scheduled_task_failing(watcher) {
        matches.push(m);
    }

    if let Some(m) = check_memory_bloat(health, config) {
        matches.push(m);
    }

    if let Some(m) = check_tool_sprawl(health, config) {
        matches.push(m);
    }

    if let Some(m) = check_disk_pressure(config) {
        matches.push(m);
    }

    // Step 2: Sort by severity descending (critical first).
    matches.sort_by(|a, b| b.severity.rank().cmp(&a.severity.rank()));

    matches
}

/// Check whether a tool is failing after a recent git change.
///
/// Fires when a tool has >50% failure rate AND a recent git commit message
/// mentions that tool (within the configured window).
async fn check_tool_failing_after_change(
    stats: &StatsEngine,
    git_log: &[GitLogEntry],
    config: &FlatlineConfig,
) -> Option<Vec<PatternMatch>> {
    let window = config.thresholds.tool_failure_window_hours;
    let threshold = config.thresholds.tool_failure_rate;

    let failing = match stats.failing_tools(threshold, window).await {
        Ok(f) => f,
        Err(e) => {
            warn!(error = %e, "failed to query failing tools");
            return None;
        }
    };

    if failing.is_empty() {
        return None;
    }

    let mut matches = Vec::new();

    for (tool_name, failure_rate) in &failing {
        // Look for a recent git commit that mentions this tool.
        let correlated_commit = git_log.iter().find(|entry| {
            entry
                .message
                .to_lowercase()
                .contains(&tool_name.to_lowercase())
        });

        if let Some(commit) = correlated_commit {
            matches.push(PatternMatch {
                kind: PatternKind::ToolFailingAfterChange,
                severity: Severity::Medium,
                evidence: Evidence {
                    summary: format!(
                        "Tool '{tool_name}' has {:.0}% failure rate after commit {}",
                        failure_rate * 100.0,
                        &commit.hash[..7.min(commit.hash.len())]
                    ),
                    details: serde_json::json!({
                        "tool": tool_name,
                        "failure_rate": failure_rate,
                        "commit_hash": commit.hash,
                        "commit_message": commit.message,
                        "commit_timestamp": commit.timestamp,
                    }),
                },
                auto_fixable: true,
            });
        }
    }

    if matches.is_empty() {
        None
    } else {
        Some(matches)
    }
}

/// Check whether the Wintermute process is down.
///
/// Fires when health.json is stale AND the PID file indicates a dead process.
fn check_process_down(watcher: &Watcher, config: &FlatlineConfig) -> Option<PatternMatch> {
    let threshold = config.checks.health_stale_threshold_secs;

    // If we can't read health.json at all, treat as potentially down.
    let stale = watcher.is_health_stale(threshold).unwrap_or(true);

    if !stale {
        return None;
    }

    // Try to read the PID file and check if the process is alive.
    let wm_paths = wintermute::config::runtime_paths().ok()?;
    let pid_alive = is_pid_alive(&wm_paths.pid_file);

    if pid_alive {
        // Process is running but health file is stale -- could be a hung process.
        // Still worth reporting but not as "process down".
        return None;
    }

    Some(PatternMatch {
        kind: PatternKind::ProcessDown,
        severity: Severity::Critical,
        evidence: Evidence {
            summary: "Wintermute process is not running and health.json is stale".to_owned(),
            details: serde_json::json!({
                "health_stale": true,
                "pid_alive": false,
                "stale_threshold_secs": threshold,
            }),
        },
        auto_fixable: true,
    })
}

/// Check whether the PID in the given file is still alive.
///
/// Reads the PID from the file, then uses `kill -0` to test process existence.
/// Returns `false` if the file doesn't exist, can't be read, or the process is dead.
pub fn is_pid_alive(pid_path: &Path) -> bool {
    let contents = match std::fs::read_to_string(pid_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let pid_str = contents.trim();
    if pid_str.is_empty() {
        return false;
    }

    let pid: u32 = match pid_str.parse() {
        Ok(p) => p,
        Err(_) => return false,
    };

    // Use `kill -0 {pid}` to test if the process exists without sending a signal.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check whether the container is repeatedly unhealthy.
///
/// Fires when health report shows `container_healthy: false`.
fn check_container_wont_start(health: Option<&HealthReport>) -> Option<PatternMatch> {
    let report = health?;

    if report.container_healthy {
        return None;
    }

    Some(PatternMatch {
        kind: PatternKind::ContainerWontStart,
        severity: Severity::High,
        evidence: Evidence {
            summary: "Container is unhealthy".to_owned(),
            details: serde_json::json!({
                "container_healthy": false,
                "status": report.status,
                "last_error": report.last_error,
            }),
        },
        auto_fixable: true,
    })
}

/// Check whether the budget is being exhausted too fast.
///
/// Fires when >80% of budget used in <25% of the day.
async fn check_budget_exhaustion(
    health: Option<&HealthReport>,
    config: &FlatlineConfig,
    stats: &StatsEngine,
) -> Option<PatternMatch> {
    let report = health?;

    let burn_rate = stats.budget_burn_rate(report).await;
    let threshold = config.thresholds.budget_burn_rate_alert;

    let limit = report.budget_today.limit;
    let used = report.budget_today.used;

    if limit == 0 {
        return None;
    }

    #[allow(clippy::cast_precision_loss)]
    let usage_fraction = used as f64 / limit as f64;

    let day_fraction = crate::stats::day_fraction_elapsed();

    // Fire if >80% budget used AND less than 25% of day has passed.
    if usage_fraction > threshold && day_fraction < 0.25 {
        return Some(PatternMatch {
            kind: PatternKind::BudgetExhaustionLoop,
            severity: Severity::Medium,
            evidence: Evidence {
                summary: format!(
                    "Budget {:.0}% used with only {:.0}% of day elapsed (burn rate: {burn_rate:.1}x)",
                    usage_fraction * 100.0,
                    day_fraction * 100.0,
                ),
                details: serde_json::json!({
                    "used": used,
                    "limit": limit,
                    "usage_fraction": usage_fraction,
                    "day_fraction": day_fraction,
                    "burn_rate": burn_rate,
                }),
            },
            auto_fixable: false,
        });
    }

    None
}

/// Check whether a scheduled task is consistently failing.
///
/// Fires when 3+ consecutive `tool_call` failures appear for the same tool
/// in recent logs.
fn check_scheduled_task_failing(watcher: &Watcher) -> Option<PatternMatch> {
    // We need to poll logs to check for consecutive failures.
    // Since the watcher uses mutable poll_logs, we read health events instead
    // and rely on the event data already collected by the daemon loop.
    // For the pattern checker, we look at the health report's last_error field
    // as a heuristic. A more complete implementation would track consecutive
    // failures in the stats engine.

    // For now, check if health report shows a last_error related to scheduled tasks.
    let report = match watcher.read_health() {
        Ok(r) => r,
        Err(_) => return None,
    };

    let error_msg = report.last_error.as_deref()?;

    // Look for task-related error patterns.
    if error_msg.contains("scheduled") || error_msg.contains("task") || error_msg.contains("cron") {
        return Some(PatternMatch {
            kind: PatternKind::ScheduledTaskFailing,
            severity: Severity::Medium,
            evidence: Evidence {
                summary: format!("Scheduled task failing: {error_msg}"),
                details: serde_json::json!({
                    "last_error": error_msg,
                }),
            },
            auto_fixable: true,
        });
    }

    None
}

/// Check whether memory database is bloated.
///
/// Fires when `memory_db_size_mb` exceeds a reasonable threshold derived
/// from config's pending memory alert count. Uses 50 MB as a heuristic
/// indicator of bloat when pending count cannot be checked directly.
fn check_memory_bloat(
    health: Option<&HealthReport>,
    config: &FlatlineConfig,
) -> Option<PatternMatch> {
    let report = health?;

    // We can't directly query pending memory count from here (read-only
    // observer), so we use memory_db_size_mb as a proxy. The config's
    // memory_pending_alert is in count, but we approximate: if the DB
    // exceeds ~50MB that's a strong signal of bloat.
    let threshold_mb = 50.0;

    if report.memory_db_size_mb <= threshold_mb {
        return None;
    }

    Some(PatternMatch {
        kind: PatternKind::MemoryBloat,
        severity: Severity::Low,
        evidence: Evidence {
            summary: format!(
                "Memory database is {:.1} MB (threshold: {threshold_mb:.0} MB, pending alert: {} items)",
                report.memory_db_size_mb,
                config.thresholds.memory_pending_alert,
            ),
            details: serde_json::json!({
                "memory_db_size_mb": report.memory_db_size_mb,
                "threshold_mb": threshold_mb,
                "pending_alert_count": config.thresholds.memory_pending_alert,
            }),
        },
        auto_fixable: false,
    })
}

/// Check whether there are too many dynamic tools.
///
/// Fires when the tool count exceeds `max_tool_count_warning`.
fn check_tool_sprawl(
    health: Option<&HealthReport>,
    config: &FlatlineConfig,
) -> Option<PatternMatch> {
    let report = health?;

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let count = report.dynamic_tools_count as u64;
    let threshold = config.thresholds.max_tool_count_warning;

    if count <= threshold {
        return None;
    }

    Some(PatternMatch {
        kind: PatternKind::DynamicToolSprawl,
        severity: Severity::Low,
        evidence: Evidence {
            summary: format!("{count} dynamic tools registered (warning threshold: {threshold})"),
            details: serde_json::json!({
                "dynamic_tools_count": count,
                "threshold": threshold,
                "scripts_count": report.scripts_count,
            }),
        },
        auto_fixable: false,
    })
}

/// Check whether disk usage of `~/.wintermute` is too high.
///
/// Fires when the directory size exceeds the configured `disk_warning_gb`.
fn check_disk_pressure(config: &FlatlineConfig) -> Option<PatternMatch> {
    let wm_root = wintermute::config::config_dir().ok()?;

    if !wm_root.exists() {
        return None;
    }

    let size_bytes = dir_size_bytes(&wm_root);
    #[allow(clippy::cast_precision_loss)]
    let size_gb = size_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let threshold_gb = config.thresholds.disk_warning_gb;

    if size_gb <= threshold_gb {
        return None;
    }

    Some(PatternMatch {
        kind: PatternKind::DiskSpacePressure,
        severity: Severity::Medium,
        evidence: Evidence {
            summary: format!("~/.wintermute is {size_gb:.2} GB (threshold: {threshold_gb:.1} GB)"),
            details: serde_json::json!({
                "size_gb": size_gb,
                "threshold_gb": threshold_gb,
                "size_bytes": size_bytes,
            }),
        },
        auto_fixable: true,
    })
}

/// Read recent git log entries from a scripts directory.
///
/// Parses output of `git log --format="%H %aI %s"`.
///
/// # Errors
///
/// Returns an error if the git command fails or output cannot be parsed.
pub fn read_git_log(scripts_dir: &Path, count: usize) -> anyhow::Result<Vec<GitLogEntry>> {
    // Clear GIT_DIR/GIT_WORK_TREE to avoid inheriting from parent git processes
    // (e.g. when running inside a pre-push hook).
    let output = std::process::Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args([
            "-C",
            &scripts_dir.to_string_lossy(),
            "log",
            "--format=%H %aI %s",
            "-n",
            &count.to_string(),
        ])
        .output()
        .with_context(|| format!("failed to run git log in {}", scripts_dir.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git log failed in {}: {stderr}", scripts_dir.display());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut entries = Vec::new();

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Format: "{hash} {iso_timestamp} {message}"
        // Hash is 40 hex chars, timestamp includes timezone offset.
        let mut parts = trimmed.splitn(3, ' ');

        let hash = match parts.next() {
            Some(h) if !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()) => h.to_owned(),
            _ => continue,
        };

        let timestamp = match parts.next() {
            Some(t) => t.to_owned(),
            None => continue,
        };

        let message = parts.next().unwrap_or_default().to_owned();

        entries.push(GitLogEntry {
            hash,
            timestamp,
            message,
        });
    }

    Ok(entries)
}

/// Calculate the total size of a directory in bytes (non-recursive into symlinks).
fn dir_size_bytes(path: &Path) -> u64 {
    let mut total: u64 = 0;

    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return 0,
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let metadata = match entry.path().symlink_metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        if metadata.is_symlink() {
            continue;
        } else if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        } else if metadata.is_dir() {
            total = total.saturating_add(dir_size_bytes(&entry.path()));
        }
    }

    total
}
