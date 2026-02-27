//! Heartbeat: scheduled tasks, health monitoring, and backup automation.
//!
//! Runs as a background Tokio task, ticking at a configurable interval.
//! Each tick evaluates cron schedules, dispatches due tasks, performs
//! health checks, and writes a health report to disk.

pub mod backup;
pub mod digest;
pub mod health;
pub mod proactive;
pub mod scheduler;
pub mod tool_review;

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use crate::agent::budget::DailyBudget;
use crate::agent::identity::{self, IdentitySnapshot};
use crate::agent::{SessionRouter, TelegramOutbound};
use crate::config::{AgentConfig, Config, RuntimePaths};
use crate::executor::Executor;
use crate::memory::{MemoryEngine, MemoryStatus};
use crate::providers::router::ModelRouter;
use crate::tools::browser::BrowserMode;
use crate::tools::ToolRouter;

/// Shared dependencies for the heartbeat runner.
pub struct HeartbeatDeps {
    /// Human-owned configuration.
    pub config: Arc<Config>,
    /// Agent-owned configuration.
    pub agent_config: Arc<AgentConfig>,
    /// Memory engine for persistence and health stats.
    pub memory: Arc<MemoryEngine>,
    /// Executor for container health checks.
    pub executor: Arc<dyn Executor>,
    /// Tool router for scheduled task execution.
    pub tool_router: Arc<ToolRouter>,
    /// Model router for provider status.
    pub router: Arc<ModelRouter>,
    /// Shared daily budget for cost tracking.
    pub daily_budget: Arc<DailyBudget>,
    /// Channel for outbound Telegram messages.
    pub telegram_tx: mpsc::Sender<TelegramOutbound>,
    /// Telegram user ID for heartbeat notifications (first allowed user).
    pub notify_user_id: i64,
    /// Resolved runtime paths.
    pub paths: RuntimePaths,
    /// Session router for active session count.
    pub session_router: Arc<SessionRouter>,
    /// Detected browser mode for SID generation.
    pub browser_mode: BrowserMode,
}

/// Run the heartbeat background loop.
///
/// Ticks every `interval_secs` from [`crate::config::HeartbeatConfig`]. Each tick evaluates
/// cron schedules for due tasks, runs health checks, and writes `health.json`.
///
/// Exits when the shutdown signal is received or the watch channel closes.
pub async fn run_heartbeat(
    deps: HeartbeatDeps,
    start_time: Instant,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let interval_secs = deps.agent_config.heartbeat.interval_secs;
    info!(interval_secs, "heartbeat started");

    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    let mut scheduler_state = scheduler::SchedulerState::new();
    let mut tick_count: u64 = 0;
    let mut last_proactive_check: Option<Instant> = None;

    // Skip the first immediate tick.
    interval.tick().await;

    // Generate SID immediately on startup.
    regenerate_sid(&deps, start_time).await;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                tick_count = tick_count.saturating_add(1);
                run_tick(&deps, &mut scheduler_state, start_time).await;

                // Regenerate SID every 5 ticks (~5 minutes at 60s interval).
                if tick_count.is_multiple_of(5) {
                    regenerate_sid(&deps, start_time).await;
                }

                // Proactive behavior check.
                if deps.agent_config.heartbeat.proactive {
                    maybe_run_proactive_check(
                        &deps,
                        start_time,
                        &mut last_proactive_check,
                    )
                    .await;
                }
            }
            result = shutdown_rx.changed() => {
                if result.is_err() || *shutdown_rx.borrow() {
                    info!("heartbeat shutting down");
                    break;
                }
            }
        }
    }

    info!("heartbeat stopped");
}

/// Run a proactive check if enough time has elapsed since the last one.
async fn maybe_run_proactive_check(
    deps: &HeartbeatDeps,
    start_time: Instant,
    last_check: &mut Option<Instant>,
) {
    let proactive_interval = Duration::from_secs(
        u64::from(deps.agent_config.heartbeat.proactive_interval_mins).saturating_mul(60),
    );
    let should_check = last_check.is_none_or(|last| last.elapsed() >= proactive_interval);
    if !should_check {
        return;
    }

    let now_str = chrono::Utc::now()
        .format("%Y-%m-%d %H:%M:%S UTC")
        .to_string();
    let session_count = deps.session_router.session_count().await;
    let mut context_summary = format!(
        "Current time: {now_str}\nUptime: {:?}\nActive sessions: {session_count}",
        start_time.elapsed(),
    );

    // Append active brief context for proactive awareness (e.g. stale brief detection)
    match crate::messaging::brief::all_active_briefs(deps.memory.pool()).await {
        Ok(briefs) if !briefs.is_empty() => {
            let brief_summary = crate::messaging::brief::active_briefs_summary(&briefs);
            context_summary.push('\n');
            context_summary.push_str(&brief_summary);
        }
        Ok(_) => {}
        Err(e) => {
            warn!(error = %e, "failed to load active briefs for proactive check");
        }
    }

    match proactive::run_proactive_check(
        &deps.router,
        &deps.daily_budget,
        deps.agent_config.heartbeat.proactive_budget,
        &context_summary,
    )
    .await
    {
        Ok(Some(action)) => {
            let redacted = deps.tool_router.redactor().redact(&action);
            let msg = TelegramOutbound {
                user_id: deps.notify_user_id,
                text: Some(redacted),
                file_path: None,
                approval_keyboard: None,
            };
            if let Err(e) = deps.telegram_tx.send(msg).await {
                warn!(error = %e, "failed to send proactive action");
            }
        }
        Ok(None) => {} // No action needed.
        Err(e) => {
            warn!(error = %e, "proactive check failed");
        }
    }

    *last_check = Some(Instant::now());
}

/// Execute a single heartbeat tick.
async fn run_tick(
    deps: &HeartbeatDeps,
    scheduler_state: &mut scheduler::SchedulerState,
    start_time: Instant,
) {
    let now = chrono::Utc::now();

    // 1. Check for due scheduled tasks.
    let due = scheduler::due_tasks(&deps.agent_config.scheduled_tasks, scheduler_state, now);

    for task_config in due {
        match scheduler::execute_task(task_config, deps, scheduler_state).await {
            Ok(outcome) => {
                info!(
                    task = %outcome.name,
                    success = outcome.success,
                    duration_ms = u64::try_from(outcome.duration.as_millis()).unwrap_or(u64::MAX),
                    "scheduled task completed"
                );
            }
            Err(e) => {
                error!(
                    task = %task_config.name,
                    error = %e,
                    "scheduled task failed"
                );
            }
        }
    }

    // 2. Health check and report.
    let health_path = deps.paths.root.join("health.json");
    let report = health::check_health(deps, start_time).await;

    if let Err(e) = health::write_health_file(&report, &health_path).await {
        warn!(error = %e, "failed to write health.json");
    }
}

/// Regenerate the System Identity Document (IDENTITY.md).
async fn regenerate_sid(deps: &HeartbeatDeps, start_time: Instant) {
    let active_count = deps
        .memory
        .count_by_status(MemoryStatus::Active)
        .await
        .unwrap_or(0);
    let pending_count = deps
        .memory
        .count_by_status(MemoryStatus::Pending)
        .await
        .unwrap_or(0);

    let dynamic_tool_count = deps.tool_router.dynamic_tool_count();

    // Resolve oracle model from roles config.
    let oracle_model = deps.config.models.roles.get("oracle").cloned();

    // Count docs/ files.
    let docs_count = std::fs::read_dir(&deps.paths.docs_dir)
        .map(|entries| entries.filter_map(|e| e.ok()).count())
        .unwrap_or(0);

    // Build scheduled task summaries.
    let scheduled_task_summaries: Vec<String> = deps
        .agent_config
        .scheduled_tasks
        .iter()
        .filter(|t| t.enabled)
        .map(|t| {
            if let Some(ref builtin) = t.builtin {
                format!("{} (builtin: {builtin}, cron: {})", t.name, t.cron)
            } else if let Some(ref tool) = t.tool {
                format!("{} (tool: {tool}, cron: {})", t.name, t.cron)
            } else {
                format!("{} (cron: {})", t.name, t.cron)
            }
        })
        .collect();

    // Build dynamic tool summaries with _meta stats.
    let dynamic_tool_summaries: Vec<(String, String, u64, f64)> = deps
        .tool_router
        .registry()
        .all_schemas()
        .into_iter()
        .map(|s| {
            let (invocations, success_rate) = s
                .meta
                .map(|m| (m.invocations, m.success_rate))
                .unwrap_or((0, 1.0));
            (s.name, s.description, invocations, success_rate)
        })
        .collect();

    let snap = IdentitySnapshot {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        model_id: deps.config.models.default.clone(),
        executor_kind: deps.executor.kind(),
        core_tool_count: crate::tools::core::core_tool_definitions().len(),
        dynamic_tool_count,
        active_memory_count: active_count,
        pending_memory_count: pending_count,
        has_vector_search: deps.memory.has_embedder(),
        session_budget_limit: deps.config.budget.max_tokens_per_session,
        daily_budget_limit: deps.config.budget.max_tokens_per_day,
        uptime: start_time.elapsed(),
        agent_name: deps.agent_config.personality.name.clone(),
        browser_mode: deps.browser_mode,
        oracle_model,
        soul_modification_mode: deps.agent_config.personality.soul_modification,
        docs_count,
        scheduled_task_summaries,
        dynamic_tool_summaries,
    };

    let content = identity::render_identity(&snap);
    if let Err(e) = identity::write_identity_file(&content, &deps.paths.identity_md) {
        warn!(error = %e, "failed to write IDENTITY.md");
    }
}
