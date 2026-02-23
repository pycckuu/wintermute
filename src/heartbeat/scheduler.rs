//! Cron evaluation and scheduled task dispatch.
//!
//! Evaluates cron expressions from `agent.toml` scheduled tasks and dispatches
//! due tasks. Builtin tasks (like "backup") are handled internally. Dynamic
//! tool tasks execute via [`ToolRouter`].

use std::collections::HashMap;
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::Context;
use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

use crate::agent::TelegramOutbound;
use crate::config::ScheduledTaskConfig;
use crate::telegram::ui::escape_html;

use super::HeartbeatDeps;

/// Tracks last-run timestamps for scheduled tasks.
#[derive(Debug)]
pub struct SchedulerState {
    /// Map of task name to last execution time.
    last_run: HashMap<String, DateTime<Utc>>,
}

impl SchedulerState {
    /// Create a new scheduler state with no recorded runs.
    pub fn new() -> Self {
        Self {
            last_run: HashMap::new(),
        }
    }

    /// Record that a task was executed at the given time.
    pub fn record_run(&mut self, name: &str, at: DateTime<Utc>) {
        self.last_run.insert(name.to_owned(), at);
    }

    /// Get the last run time for a task.
    pub fn last_run_for(&self, name: &str) -> Option<&DateTime<Utc>> {
        self.last_run.get(name)
    }
}

impl Default for SchedulerState {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of a scheduled task execution.
#[derive(Debug)]
pub struct TaskOutcome {
    /// Task name.
    pub name: String,
    /// Whether the task succeeded.
    pub success: bool,
    /// Output or error message.
    pub output: String,
    /// Tokens consumed (0 for non-LLM tasks).
    pub tokens_used: u64,
    /// Wall-clock duration.
    pub duration: Duration,
}

/// Check which tasks are due for execution this tick.
///
/// A task is due if:
/// 1. It is enabled.
/// 2. Its cron expression matches a time between the last run and now.
/// 3. It has not been run within the current cron interval.
pub fn due_tasks<'a>(
    tasks: &'a [ScheduledTaskConfig],
    state: &SchedulerState,
    now: DateTime<Utc>,
) -> Vec<&'a ScheduledTaskConfig> {
    tasks
        .iter()
        .filter(|task| {
            if !task.enabled {
                return false;
            }

            let schedule = match cron::Schedule::from_str(&task.cron) {
                Ok(s) => s,
                Err(e) => {
                    warn!(
                        task = %task.name,
                        cron = %task.cron,
                        error = %e,
                        "invalid cron expression, skipping task"
                    );
                    return false;
                }
            };

            // For never-run tasks, use epoch so the first cron match triggers.
            let after = state
                .last_run_for(&task.name)
                .copied()
                .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);

            // Check if there's a cron trigger between the last run and now.
            schedule.after(&after).take(1).any(|next| next <= now)
        })
        .collect()
}

/// Execute a scheduled task.
///
/// Dispatches to the appropriate handler based on task configuration:
/// - Builtin tasks (e.g. "backup") are handled internally.
/// - Tool tasks execute via [`ToolRouter`].
///
/// # Errors
///
/// Returns an error if the task execution fails.
pub async fn execute_task(
    task: &ScheduledTaskConfig,
    deps: &HeartbeatDeps,
    state: &mut SchedulerState,
) -> anyhow::Result<TaskOutcome> {
    let start = Instant::now();
    info!(task = %task.name, "executing scheduled task");

    let result = if let Some(ref builtin) = task.builtin {
        execute_builtin(builtin, deps).await
    } else if let Some(ref tool_name) = task.tool {
        execute_tool(tool_name, task, deps).await
    } else {
        Err(anyhow::anyhow!(
            "task '{}' has neither builtin nor tool configured",
            task.name
        ))
    };

    let duration = start.elapsed();
    state.record_run(&task.name, Utc::now());

    let outcome = match result {
        Ok(output) => TaskOutcome {
            name: task.name.clone(),
            success: true,
            output,
            tokens_used: 0,
            duration,
        },
        Err(e) => TaskOutcome {
            name: task.name.clone(),
            success: false,
            output: e.to_string(),
            tokens_used: 0,
            duration,
        },
    };

    // Notify user if configured.
    if task.notify {
        let status = if outcome.success {
            "completed"
        } else {
            "failed"
        };
        let text = format!(
            "<b>Scheduled task:</b> {}\n<b>Status:</b> {}\n<b>Output:</b> {}",
            escape_html(&outcome.name),
            status,
            escape_html(&outcome.output[..outcome.output.len().min(500)])
        );
        let msg = TelegramOutbound {
            user_id: deps.notify_user_id,
            text: Some(text),
            file_path: None,
            approval_keyboard: None,
        };
        if let Err(e) = deps.telegram_tx.send(msg).await {
            warn!(error = %e, "failed to send task notification");
        }
    }

    Ok(outcome)
}

/// Execute a builtin task by name.
async fn execute_builtin(name: &str, deps: &HeartbeatDeps) -> anyhow::Result<String> {
    match name {
        "backup" => {
            let result = super::backup::create_backup(
                &deps.paths.scripts_dir,
                deps.memory.pool(),
                &deps.paths.backups_dir,
            )
            .await?;
            Ok(format!("backup created at {}", result.backup_dir.display()))
        }
        other => Err(anyhow::anyhow!("unknown builtin task: {other}")),
    }
}

/// Execute a dynamic tool task via the ToolRouter.
async fn execute_tool(
    tool_name: &str,
    task: &ScheduledTaskConfig,
    deps: &HeartbeatDeps,
) -> anyhow::Result<String> {
    // Budget check for tool tasks.
    if let Some(budget_tokens) = task.budget_tokens {
        deps.daily_budget
            .check(budget_tokens)
            .context("scheduled task budget exceeded")?;
    }

    debug!(tool = %tool_name, "executing scheduled tool task");

    let input = serde_json::json!({});
    let result = deps.tool_router.execute(tool_name, &input).await;

    if result.is_error {
        Err(anyhow::anyhow!("tool error: {}", result.content))
    } else {
        Ok(result.content)
    }
}
