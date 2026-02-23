//! Fix lifecycle: propose, apply, verify.
//!
//! All corrective actions use a security-constrained allowlist (`FixAction` enum).
//! Only `std::process::Command` usage in the entire crate lives here (aside from
//! `patterns::is_pid_alive` and `patterns::read_git_log`).

use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use wintermute::config::RuntimePaths;

use crate::config::FlatlineConfig;
use crate::db::FixRecord;
use crate::patterns::{PatternKind, PatternMatch};
use crate::watcher::Watcher;

/// Security-constrained allowlist of fix actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixAction {
    /// Restart the Wintermute process.
    RestartProcess,
    /// Reset the Docker sandbox.
    ResetSandbox,
    /// Revert a specific git commit in /scripts.
    GitRevert {
        /// The commit hash to revert.
        commit_hash: String,
    },
    /// Quarantine a tool by renaming its JSON file.
    QuarantineTool {
        /// Name of the tool to quarantine.
        tool_name: String,
    },
    /// Disable a scheduled task in agent.toml.
    DisableScheduledTask {
        /// Name of the task to disable.
        task_name: String,
    },
    /// Prune old log files.
    PruneLogs {
        /// Delete logs older than this many days.
        retention_days: u64,
    },
    /// No action, just report to user.
    ReportOnly {
        /// Message to show the user.
        message: String,
    },
}

/// Status of a fix through its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixStatus {
    /// Issue has been detected.
    Detected,
    /// Cause has been diagnosed.
    Diagnosed,
    /// Fix has been proposed to the user.
    Proposed,
    /// User has approved the fix.
    Approved,
    /// Fix has been applied.
    Applied,
    /// Fix has been verified as effective.
    Verified,
    /// Fix application or verification failed.
    Failed,
}

/// Create a fix record from a pattern match.
///
/// Maps each `PatternKind` to an appropriate `FixAction` and constructs a
/// [`FixRecord`] ready for persistence and application.
pub fn propose_fix(pattern: &PatternMatch, config: &FlatlineConfig) -> FixRecord {
    let now = chrono::Utc::now().to_rfc3339();
    let id = format!("fix-{}", uuid::Uuid::new_v4());

    let (action, diagnosis) = match pattern.kind {
        PatternKind::ToolFailingAfterChange => {
            let tool_name = evidence_str(&pattern.evidence, "tool", "unknown");
            let commit_hash = evidence_str(&pattern.evidence, "commit_hash", "");

            if config.auto_fix.quarantine_failing_tools && !commit_hash.is_empty() {
                (
                    FixAction::QuarantineTool {
                        tool_name: tool_name.clone(),
                    },
                    format!("Tool '{tool_name}' failing after commit {commit_hash}; quarantining"),
                )
            } else {
                (
                    FixAction::ReportOnly {
                        message: format!("Tool '{tool_name}' is failing after a recent change"),
                    },
                    format!("Tool '{tool_name}' failing after recent change"),
                )
            }
        }

        PatternKind::ProcessDown => {
            if config.auto_fix.restart_on_crash {
                (
                    FixAction::RestartProcess,
                    "Wintermute process is down; restarting".to_owned(),
                )
            } else {
                (
                    FixAction::ReportOnly {
                        message: "Wintermute process is not running".to_owned(),
                    },
                    "Wintermute process is down".to_owned(),
                )
            }
        }

        PatternKind::ContainerWontStart => (
            FixAction::ResetSandbox,
            "Container is unhealthy; resetting sandbox".to_owned(),
        ),

        PatternKind::BudgetExhaustionLoop => {
            let summary = pattern.evidence.summary.clone();
            (
                FixAction::ReportOnly {
                    message: summary.clone(),
                },
                summary,
            )
        }

        PatternKind::ScheduledTaskFailing => {
            let task_name = evidence_str(&pattern.evidence, "task_name", "unknown");

            if config.auto_fix.disable_failing_tasks {
                (
                    FixAction::DisableScheduledTask {
                        task_name: task_name.clone(),
                    },
                    format!("Scheduled task '{task_name}' consistently failing; disabling"),
                )
            } else {
                (
                    FixAction::ReportOnly {
                        message: format!("Scheduled task '{task_name}' is consistently failing"),
                    },
                    format!("Scheduled task '{task_name}' consistently failing"),
                )
            }
        }

        PatternKind::MemoryBloat => {
            let summary = pattern.evidence.summary.clone();
            (
                FixAction::ReportOnly {
                    message:
                        "Memory database appears bloated. Consider reviewing pending memories."
                            .to_owned(),
                },
                summary,
            )
        }

        PatternKind::DynamicToolSprawl => {
            let summary = pattern.evidence.summary.clone();
            (
                FixAction::ReportOnly {
                    message: "Too many dynamic tools. Consider archiving unused tools.".to_owned(),
                },
                summary,
            )
        }

        PatternKind::DiskSpacePressure => (
            FixAction::PruneLogs { retention_days: 7 },
            "Disk space pressure; pruning old logs".to_owned(),
        ),
    };

    let action_json = serde_json::to_string(&action).unwrap_or_else(|_| "\"unknown\"".to_owned());

    FixRecord {
        id,
        detected_at: now,
        pattern: Some(format!("{:?}", pattern.kind)),
        diagnosis: Some(diagnosis),
        action: Some(action_json),
        applied_at: None,
        verified: None,
        user_notified: false,
    }
}

/// Extract a string field from pattern evidence details, with a fallback default.
fn evidence_str(evidence: &crate::patterns::Evidence, key: &str, default: &str) -> String {
    evidence
        .details
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(default)
        .to_owned()
}

/// Apply a fix action to the system.
///
/// This is the ONLY place `std::process::Command` is used for fix actions.
/// Each action variant maps to a specific, validated system command.
///
/// # Errors
///
/// Returns an error if the command fails or the action cannot be performed.
pub async fn apply_fix(fix: &FixRecord, paths: &RuntimePaths) -> anyhow::Result<()> {
    let action_str = fix
        .action
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("fix record has no action"))?;

    let action: FixAction =
        serde_json::from_str(action_str).context("failed to parse fix action")?;

    match action {
        FixAction::RestartProcess => apply_restart_process(paths).await,
        FixAction::ResetSandbox => apply_reset_sandbox().await,
        FixAction::GitRevert { commit_hash } => {
            apply_git_revert(&commit_hash, &paths.scripts_dir).await
        }
        FixAction::QuarantineTool { tool_name } => {
            apply_quarantine_tool(&tool_name, &paths.scripts_dir).await
        }
        FixAction::DisableScheduledTask { task_name } => {
            apply_disable_scheduled_task(&task_name, &paths.agent_toml).await
        }
        FixAction::PruneLogs { retention_days } => {
            apply_prune_logs(retention_days, &paths.root).await
        }
        FixAction::ReportOnly { message } => {
            info!(message = %message, "report-only fix, no action taken");
            Ok(())
        }
    }
}

/// Verify that a fix was effective.
///
/// Checks vary by action type. Returns `true` if the fix appears to have worked.
///
/// # Errors
///
/// Returns an error if verification cannot be performed.
pub async fn verify_fix(fix: &FixRecord, watcher: &Watcher) -> anyhow::Result<bool> {
    let action_str = match fix.action.as_deref() {
        Some(s) => s,
        None => return Ok(false),
    };

    let action: FixAction = match serde_json::from_str(action_str) {
        Ok(a) => a,
        Err(_) => return Ok(false),
    };

    match action {
        FixAction::RestartProcess => {
            // Check if health.json is fresh again.
            // Use a generous threshold since the process just restarted.
            match watcher.is_health_stale(300) {
                Ok(stale) => Ok(!stale),
                Err(_) => Ok(false),
            }
        }
        FixAction::ResetSandbox => {
            // Check if container is healthy.
            match watcher.read_health() {
                Ok(report) => Ok(report.container_healthy),
                Err(_) => Ok(false),
            }
        }
        FixAction::GitRevert { .. } | FixAction::QuarantineTool { .. } => {
            // For tool-related fixes, we consider success if health.json
            // is readable and no new errors. Full verification would need
            // to wait for the next tool invocation.
            match watcher.read_health() {
                Ok(_) => Ok(true),
                Err(_) => Ok(false),
            }
        }
        FixAction::DisableScheduledTask { .. } => {
            // The task is disabled; verification is that no more errors occur.
            // For now, consider it verified immediately since the config change
            // prevents future executions.
            Ok(true)
        }
        FixAction::PruneLogs { .. } => {
            // Log pruning is always considered verified.
            Ok(true)
        }
        FixAction::ReportOnly { .. } => {
            // Report-only actions are always "verified".
            Ok(true)
        }
    }
}

/// Validate that a commit hash contains only hex digits.
///
/// # Errors
///
/// Returns an error if the hash is empty or contains non-hex characters.
pub fn validate_commit_hash(hash: &str) -> anyhow::Result<()> {
    if hash.is_empty() || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("invalid commit hash: must be non-empty hex string");
    }
    Ok(())
}

// -- Private action implementations --

/// Restart the Wintermute process by sending SIGTERM and then starting it again.
async fn apply_restart_process(paths: &RuntimePaths) -> anyhow::Result<()> {
    // Step 1: Read PID and send SIGTERM.
    let pid_file = paths.pid_file.clone();
    let pid_contents = std::fs::read_to_string(&pid_file)
        .with_context(|| format!("failed to read PID file at {}", pid_file.display()))?;

    let pid_str = pid_contents.trim();
    if !pid_str.is_empty() {
        let pid: u32 = pid_str
            .parse()
            .with_context(|| format!("invalid PID value in pid file: {pid_str:?}"))?;
        info!(pid, "sending SIGTERM to wintermute");
        let pid_string = pid.to_string();
        tokio::task::spawn_blocking(move || {
            std::process::Command::new("kill")
                .args([&pid_string])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
        })
        .await
        .context("kill task panicked")?
        .ok();

        // Wait 5 seconds for graceful shutdown.
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }

    // Step 2: Start wintermute as a background process.
    info!("starting wintermute");
    tokio::task::spawn_blocking(|| {
        std::process::Command::new("wintermute")
            .arg("start")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    })
    .await
    .context("wintermute start task panicked")?
    .context("failed to spawn wintermute start")?;

    Ok(())
}

/// Reset the Docker sandbox via `wintermute reset`.
async fn apply_reset_sandbox() -> anyhow::Result<()> {
    info!("resetting sandbox");
    let status = tokio::task::spawn_blocking(|| {
        std::process::Command::new("wintermute")
            .arg("reset")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    })
    .await
    .context("wintermute reset task panicked")?
    .context("failed to run wintermute reset")?;

    if !status.success() {
        anyhow::bail!("wintermute reset exited with status {status}");
    }

    Ok(())
}

/// Revert a git commit in the scripts directory.
async fn apply_git_revert(commit_hash: &str, scripts_dir: &Path) -> anyhow::Result<()> {
    validate_commit_hash(commit_hash)?;

    info!(commit = %commit_hash, "reverting git commit");
    let scripts_str = scripts_dir.to_string_lossy().to_string();
    let hash = commit_hash.to_owned();
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("git")
            .args(["-C", &scripts_str, "revert", "--no-edit", &hash])
            .output()
    })
    .await
    .context("git revert task panicked")?
    .context("failed to run git revert")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git revert failed: {stderr}");
    }

    Ok(())
}

/// Quarantine a tool by renaming its JSON file.
async fn apply_quarantine_tool(tool_name: &str, scripts_dir: &Path) -> anyhow::Result<()> {
    // Validate tool name doesn't contain path traversal or control characters.
    if tool_name.contains('/')
        || tool_name.contains('\\')
        || tool_name.contains("..")
        || tool_name.chars().any(|c| c.is_control())
        || tool_name.len() > 128
    {
        anyhow::bail!("invalid tool name: contains disallowed characters or exceeds length limit");
    }

    let source = scripts_dir.join(format!("{tool_name}.json"));
    let target = scripts_dir.join(format!("{tool_name}.json.quarantined"));

    if !source.exists() {
        warn!(tool = %tool_name, "tool JSON file not found, skipping quarantine");
        return Ok(());
    }

    info!(tool = %tool_name, from = %source.display(), to = %target.display(), "quarantining tool");
    tokio::fs::rename(&source, &target).await.with_context(|| {
        format!(
            "failed to quarantine tool: rename {} to {}",
            source.display(),
            target.display()
        )
    })?;

    Ok(())
}

/// Disable a scheduled task by editing agent.toml.
async fn apply_disable_scheduled_task(
    task_name: &str,
    agent_toml_path: &Path,
) -> anyhow::Result<()> {
    if task_name.contains('/')
        || task_name.contains('\\')
        || task_name.contains("..")
        || task_name.chars().any(|c| c.is_control())
    {
        anyhow::bail!("invalid task name: contains disallowed characters");
    }

    let contents = tokio::fs::read_to_string(agent_toml_path)
        .await
        .with_context(|| format!("failed to read agent.toml at {}", agent_toml_path.display()))?;

    // Parse as a TOML document for editing.
    let mut doc: toml::Value = toml::from_str(&contents).context("failed to parse agent.toml")?;

    // Find the scheduled task by name and set enabled = false.
    let tasks = doc
        .get_mut("scheduled_tasks")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("no scheduled_tasks array in agent.toml"))?;

    let mut found = false;
    for task in tasks.iter_mut() {
        let matches_name = task
            .get("name")
            .and_then(|v| v.as_str())
            .is_some_and(|name| name == task_name);

        if matches_name {
            if let Some(table) = task.as_table_mut() {
                table.insert("enabled".to_owned(), toml::Value::Boolean(false));
                found = true;
            }
        }
    }

    if !found {
        anyhow::bail!("scheduled task '{task_name}' not found in agent.toml");
    }

    // Serialize back and write.
    let updated = toml::to_string_pretty(&doc).context("failed to serialize agent.toml")?;

    info!(task = %task_name, "disabling scheduled task in agent.toml");
    tokio::fs::write(agent_toml_path, updated.as_bytes())
        .await
        .with_context(|| {
            format!(
                "failed to write agent.toml at {}",
                agent_toml_path.display()
            )
        })?;

    Ok(())
}

/// Prune log files older than the retention period.
async fn apply_prune_logs(retention_days: u64, wintermute_root: &Path) -> anyhow::Result<()> {
    let logs_dir = wintermute_root.join("logs");

    if !logs_dir.exists() {
        return Ok(());
    }

    let now = std::time::SystemTime::now();
    let retention = std::time::Duration::from_secs(retention_days.saturating_mul(86400));

    let entries = std::fs::read_dir(&logs_dir)
        .with_context(|| format!("failed to read logs directory {}", logs_dir.display()))?;

    let mut pruned_count: u64 = 0;

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        if !metadata.is_file() {
            continue;
        }

        // Only prune known log file types.
        let path = entry.path();
        let is_log_file = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext, "jsonl" | "log" | "txt"));
        if !is_log_file {
            continue;
        }

        let modified = match metadata.modified() {
            Ok(m) => m,
            Err(_) => continue,
        };

        let age = now.duration_since(modified).unwrap_or_default();
        if age > retention {
            if let Err(e) = std::fs::remove_file(&path) {
                warn!(path = %path.display(), error = %e, "failed to prune log file");
            } else {
                pruned_count = pruned_count.saturating_add(1);
            }
        }
    }

    info!(count = pruned_count, retention_days, "pruned old log files");
    Ok(())
}
