//! Flatline CLI entry point.
//!
//! Provides `start` and `check` subcommands for running the supervisor daemon
//! or performing a single diagnostic check.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing::{debug, info, warn};

use flatline::config::{flatline_paths, load_flatline_config};
use flatline::db::StateDb;
use flatline::reporter::Reporter;
use flatline::stats::StatsEngine;
use flatline::watcher::Watcher;
use flatline::{diagnosis, fixer, patterns};

/// Flatline — supervisor process for the Wintermute AI agent.
#[derive(Parser)]
#[command(name = "flatline", version, about)]
struct Cli {
    /// Subcommand to execute.
    #[command(subcommand)]
    command: Command,
}

/// Available CLI subcommands.
#[derive(Subcommand)]
enum Command {
    /// Run the Flatline supervisor daemon.
    Start,
    /// Run a single diagnostic check and exit.
    Check,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Start => handle_start().await,
        Command::Check => handle_check().await,
    }
}

/// Run the Flatline supervisor daemon.
async fn handle_start() -> anyhow::Result<()> {
    // Resolve paths.
    let wm_paths = wintermute::config::runtime_paths()?;
    let fl_paths = flatline_paths()?;
    let flatline_config_path = wm_paths.root.join("flatline.toml");

    // Ensure Flatline directories exist.
    std::fs::create_dir_all(&fl_paths.root)
        .with_context(|| format!("failed to create {}", fl_paths.root.display()))?;
    std::fs::create_dir_all(&fl_paths.diagnoses_dir)
        .with_context(|| format!("failed to create {}", fl_paths.diagnoses_dir.display()))?;
    std::fs::create_dir_all(&fl_paths.patches_dir)
        .with_context(|| format!("failed to create {}", fl_paths.patches_dir.display()))?;

    // Set up production logging (JSON file + stderr).
    let logs_dir = fl_paths.root.join("logs");
    let _logging_guard = wintermute::logging::init_production(&logs_dir)?;

    // Load configs.
    let config = load_flatline_config(&flatline_config_path)
        .with_context(|| format!("failed to load {}", flatline_config_path.display()))?;

    let wm_config = wintermute::config::load_default_config()
        .with_context(|| format!("failed to load {}", wm_paths.config_toml.display()))?;

    let credentials = wintermute::credentials::load_default_credentials()
        .with_context(|| format!("failed to load {}", wm_paths.env_file.display()))?;

    // Create ModelRouter for the "flatline" role.
    let router =
        wintermute::providers::router::ModelRouter::from_config(&wm_config.models, &credentials)
            .context("failed to create model router")?;

    // Create Redactor.
    let redactor = wintermute::executor::redactor::Redactor::new(credentials.known_secrets());

    // Create DailyBudget for Flatline's own token usage.
    let daily_budget = Arc::new(wintermute::agent::budget::DailyBudget::new(
        config.budget.max_tokens_per_day,
    ));

    // Open state database.
    let db = Arc::new(StateDb::open(&fl_paths.state_db).await?);

    // Create Watcher.
    let log_dir = wm_paths.data_dir.join("logs");
    let mut watcher = Watcher::new(log_dir, wm_paths.health_json.clone());

    // Create StatsEngine.
    let stats = StatsEngine::new(Arc::clone(&db));

    // Create Telegram Reporter.
    let bot_token = credentials
        .get(&config.telegram.bot_token_env)
        .unwrap_or_default()
        .to_owned();

    let mut reporter = Reporter::new(
        &bot_token,
        config.telegram.notify_users.clone(),
        config.reports.telegram_prefix.clone(),
        config.reports.alert_cooldown_mins,
    );

    info!(
        config = %flatline_config_path.display(),
        state_db = %fl_paths.state_db.display(),
        "flatline supervisor started"
    );

    // Track restart timestamps for rate-limiting.
    let mut restart_times: Vec<chrono::DateTime<chrono::Utc>> = Vec::new();

    // Main daemon loop.
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
        config.checks.interval_secs,
    ));

    loop {
        interval.tick().await;

        // Step 1: Poll logs.
        let events = watcher.poll_logs().unwrap_or_default();

        // Step 2: Ingest into stats.
        if let Err(e) = stats.ingest(&events).await {
            warn!(error = %e, "stats ingestion failed");
        }

        // Step 3: Read health.
        let health = watcher.read_health().ok();

        // Step 4: Read git log.
        let git_log = patterns::read_git_log(&wm_paths.scripts_dir, 20).unwrap_or_default();

        // Step 5: Evaluate patterns.
        let matches =
            patterns::evaluate_patterns(&stats, health.as_ref(), &git_log, &config, &watcher).await;

        // Step 6: Process matches.
        for m in &matches {
            process_match(
                m,
                &config,
                &db,
                &wm_paths,
                &watcher,
                &mut reporter,
                &mut restart_times,
            )
            .await;
        }

        // Step 7: If no patterns but has error events, try LLM diagnosis.
        if matches.is_empty() {
            let error_events: Vec<_> = events
                .iter()
                .filter(|e| e.level.as_deref() == Some("error"))
                .cloned()
                .collect();

            if !error_events.is_empty() {
                match diagnosis::diagnose(
                    &error_events,
                    health.as_ref(),
                    &git_log,
                    &[],
                    &router,
                    &redactor,
                    &daily_budget,
                )
                .await
                {
                    Ok(Some(d)) => {
                        debug!(
                            root_cause = %d.root_cause,
                            confidence = ?d.confidence,
                            "LLM diagnosis"
                        );
                    }
                    Ok(None) => {
                        debug!("LLM diagnosis returned no actionable result");
                    }
                    Err(e) => {
                        debug!(error = %e, "LLM diagnosis failed (non-fatal)");
                    }
                }
            }
        }

        debug!("check cycle complete");
    }
}

/// Process a single pattern match: check suppression, propose a fix,
/// optionally auto-apply, and notify via Telegram.
async fn process_match(
    m: &patterns::PatternMatch,
    config: &flatline::config::FlatlineConfig,
    db: &StateDb,
    wm_paths: &wintermute::config::RuntimePaths,
    watcher: &Watcher,
    reporter: &mut Reporter,
    restart_times: &mut Vec<chrono::DateTime<chrono::Utc>>,
) {
    // Check suppression.
    if db
        .is_suppressed(&format!("{:?}", m.kind))
        .await
        .unwrap_or(false)
    {
        debug!(pattern = ?m.kind, "pattern suppressed, skipping");
        return;
    }

    // Propose fix.
    let fix = fixer::propose_fix(m, config);
    if let Err(e) = db.insert_fix(&fix).await {
        warn!(error = %e, "failed to persist fix record");
    }

    // Auto-fix if enabled and auto-fixable.
    if m.auto_fixable && config.auto_fix.enabled {
        // Rate-limit RestartProcess actions.
        if m.kind == patterns::PatternKind::ProcessDown {
            let now = chrono::Utc::now();
            let one_hour_ago = now
                .checked_sub_signed(chrono::Duration::hours(1))
                .unwrap_or(now);
            restart_times.retain(|t| *t > one_hour_ago);

            if restart_times.len() >= config.auto_fix.max_auto_restarts_per_hour as usize {
                warn!(
                    count = restart_times.len(),
                    limit = config.auto_fix.max_auto_restarts_per_hour,
                    "restart rate limit exceeded, sending alert instead"
                );
                if let Err(e) = reporter.send_alert(m).await {
                    warn!(error = %e, "failed to send rate-limit alert");
                }
                return;
            }

            restart_times.push(now);
        }

        match fixer::apply_fix(&fix, wm_paths).await {
            Ok(()) => {
                let verified = fixer::verify_fix(&fix, watcher).await.unwrap_or(false);
                info!(
                    pattern = ?m.kind,
                    verified,
                    "auto-fix applied"
                );
                if let Err(e) = db
                    .update_fix(
                        &fix.id,
                        Some(&chrono::Utc::now().to_rfc3339()),
                        Some(verified),
                        None,
                    )
                    .await
                {
                    warn!(error = %e, "failed to update fix record");
                }
                if let Err(e) = reporter.send_fix_applied(&fix).await {
                    warn!(error = %e, "failed to send fix-applied notification");
                }
            }
            Err(e) => {
                warn!(error = %e, pattern = ?m.kind, "auto-fix failed");
                if let Err(e) = reporter.send_alert(m).await {
                    warn!(error = %e, "failed to send alert notification");
                }
            }
        }
    } else {
        // Send alert for non-auto-fixable patterns or when auto-fix is disabled.
        if let Err(e) = reporter.send_alert(m).await {
            warn!(error = %e, "failed to send alert notification");
        }
    }
}

/// Run a single diagnostic check and exit.
async fn handle_check() -> anyhow::Result<()> {
    wintermute::logging::init_cli();

    let wm_paths = wintermute::config::runtime_paths()?;
    let fl_paths = flatline_paths()?;
    let flatline_config_path = wm_paths.root.join("flatline.toml");

    // Load config.
    let config = load_flatline_config(&flatline_config_path)
        .with_context(|| format!("failed to load {}", flatline_config_path.display()))?;

    // Create Watcher.
    let log_dir = wm_paths.data_dir.join("logs");
    let watcher = Watcher::new(log_dir, wm_paths.health_json.clone());

    // Open state database (for stats queries).
    let db = Arc::new(StateDb::open(&fl_paths.state_db).await?);
    let stats = StatsEngine::new(Arc::clone(&db));

    // Step 1: Read health.
    let health = match watcher.read_health() {
        Ok(report) => {
            let json = serde_json::to_string_pretty(&report)
                .context("failed to serialize health report")?;
            info!(health = %json, "current health");
            Some(report)
        }
        Err(e) => {
            info!(error = %e, "could not read health.json (wintermute may not be running)");
            None
        }
    };

    // Step 2: Read git log.
    let git_log = patterns::read_git_log(&wm_paths.scripts_dir, 20).unwrap_or_default();

    // Step 3: Evaluate patterns.
    let matches =
        patterns::evaluate_patterns(&stats, health.as_ref(), &git_log, &config, &watcher).await;

    if matches.is_empty() {
        info!("no issues detected");
    } else {
        for m in &matches {
            info!(
                kind = ?m.kind,
                severity = ?m.severity,
                summary = %m.evidence.summary,
                auto_fixable = m.auto_fixable,
                "issue detected"
            );
        }
    }

    Ok(())
}
