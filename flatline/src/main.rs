//! Flatline CLI entry point.
//!
//! Provides `start`, `check`, and `update` subcommands for running the supervisor
//! daemon, performing a single diagnostic check, or applying updates.

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
use flatline::updater::{self, Updater};
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
    /// Check for updates and apply the latest version.
    Update {
        /// Only check for a newer version without applying it.
        #[arg(long)]
        check: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Start => handle_start().await,
        Command::Check => handle_check().await,
        Command::Update { check } => handle_update(check).await,
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
    std::fs::create_dir_all(&fl_paths.updates_dir)
        .with_context(|| format!("failed to create {}", fl_paths.updates_dir.display()))?;
    std::fs::create_dir_all(&fl_paths.pending_dir)
        .with_context(|| format!("failed to create {}", fl_paths.pending_dir.display()))?;

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

    // Start Wintermute on boot if auto-fix restart is enabled and not already running.
    if config.auto_fix.enabled
        && config.auto_fix.restart_on_crash
        && !patterns::is_pid_alive(&wm_paths.pid_file)
    {
        info!("wintermute not running, starting on boot");
        match fixer::start_wintermute(&wm_paths).await {
            Ok(()) => info!("wintermute start issued successfully"),
            Err(e) => warn!(error = %e, "failed to start wintermute on boot"),
        }
    }

    // Create the Updater.
    let updater = Updater::new(config.update.clone(), fl_paths.clone(), wm_paths.clone());

    // Track restart timestamps for rate-limiting.
    let mut restart_times: Vec<chrono::DateTime<chrono::Utc>> = Vec::new();

    // Update tracking state.
    let mut last_update_check: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut pending_release: Option<updater::ReleaseInfo> = None;
    let mut pending_db_id: i64 = 0;
    let mut update_approved: bool = false;
    let mut idle_wait_start: Option<chrono::DateTime<chrono::Utc>> = None;

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

        // Step 8: Update check (daily at configured time).
        if config.update.enabled {
            let should_check = last_update_check
                .map(|t| chrono::Utc::now().signed_duration_since(t).num_hours() >= 20)
                .unwrap_or(true);

            if should_check
                && updater::is_check_time(&config.update.check_time, config.checks.interval_secs)
            {
                match updater.check_for_update().await {
                    Ok(Some(release)) => {
                        info!(version = %release.version, "new version available");

                        // Record in DB.
                        let record = flatline::db::UpdateRecord {
                            id: 0,
                            checked_at: chrono::Utc::now().to_rfc3339(),
                            from_version: env!("CARGO_PKG_VERSION").to_owned(),
                            to_version: release.version.clone(),
                            status: "pending".to_owned(),
                            started_at: None,
                            completed_at: None,
                            rollback_reason: None,
                            migration_log: None,
                        };
                        match db.insert_update(&record).await {
                            Ok(id) => pending_db_id = id,
                            Err(e) => warn!(error = %e, "failed to record update in DB"),
                        }

                        // Download the release.
                        match updater.download_release(&release).await {
                            Ok(_paths) => {
                                info!(version = %release.version, "release downloaded");

                                // Notify user.
                                if let Err(e) = reporter
                                    .send_update_available(
                                        env!("CARGO_PKG_VERSION"),
                                        &release.version,
                                        &release.changelog,
                                        config.update.auto_apply,
                                    )
                                    .await
                                {
                                    warn!(error = %e, "failed to notify about update");
                                }

                                pending_release = Some(release);
                                if config.update.auto_apply {
                                    update_approved = true;
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to download release");
                            }
                        }

                        last_update_check = Some(chrono::Utc::now());
                    }
                    Ok(None) => {
                        debug!("no update available");
                        last_update_check = Some(chrono::Utc::now());
                    }
                    Err(e) => {
                        warn!(error = %e, "update check failed");
                        last_update_check = Some(chrono::Utc::now());
                    }
                }
            }
        }

        // Step 9: Apply pending update if approved and idle.
        if pending_release.is_some() && update_approved {
            let idle = health.as_ref().map(|h| updater.is_idle(h)).unwrap_or(false);

            if idle {
                // Clone release only when actually applying.
                let release = pending_release.clone().expect("checked is_some above");
                info!(version = %release.version, "applying update (idle window found)");
                match updater
                    .apply_update(&release, pending_db_id, &db, &mut reporter, &watcher)
                    .await
                {
                    Ok(true) => {
                        // Wintermute healthy with new version — self-update flatline.
                        if let Err(e) = reporter
                            .send_update_result(
                                env!("CARGO_PKG_VERSION"),
                                &release.version,
                                true,
                                None,
                            )
                            .await
                        {
                            warn!(error = %e, "failed to send update success notification");
                        }
                        if let Err(e) = updater.self_update(&release).await {
                            warn!(error = %e, "flatline self-update failed");
                        }
                        // self_update calls process::exit on success
                    }
                    Ok(false) => {
                        // Rolled back.
                        pending_release = None;
                        update_approved = false;
                        idle_wait_start = None;
                    }
                    Err(e) => {
                        warn!(error = %e, "update application failed");
                        pending_release = None;
                        update_approved = false;
                        idle_wait_start = None;
                    }
                }
            } else if let Some(release) = pending_release.as_ref() {
                // Not idle — track wait time.
                let wait_start = idle_wait_start.get_or_insert(chrono::Utc::now());
                let waited_hours = chrono::Utc::now()
                    .signed_duration_since(*wait_start)
                    .num_hours();

                let patience = i64::try_from(config.update.idle_patience_hours).unwrap_or(i64::MAX);

                if waited_hours >= patience {
                    info!("idle patience exhausted, deferring update");
                    if let Err(e) = reporter
                        .send_update_progress(
                            &release.version,
                            "waiting for idle window (patience exhausted, will retry tomorrow)",
                        )
                        .await
                    {
                        warn!(error = %e, "failed to send idle patience notification");
                    }
                    pending_release = None;
                    update_approved = false;
                    idle_wait_start = None;
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

/// Check for updates and optionally apply the latest version.
///
/// Downloads the dist archive (binaries + service files), stops running
/// services, swaps binaries, reinstalls service files, and restarts services.
async fn handle_update(check_only: bool) -> anyhow::Result<()> {
    wintermute::logging::init_cli();

    let wm_paths = wintermute::config::runtime_paths()?;
    let fl_paths = flatline_paths()?;
    let flatline_config_path = wm_paths.root.join("flatline.toml");

    let config = load_flatline_config(&flatline_config_path)
        .with_context(|| format!("failed to load {}", flatline_config_path.display()))?;

    // Override config: CLI update always checks regardless of enabled/pinned settings.
    let mut update_config = config.update.clone();
    update_config.enabled = true;
    update_config.pinned_version = None;

    let updater = Updater::new(update_config, fl_paths, wm_paths.clone());

    // Step 1: Check for update.
    info!("checking for updates...");
    let release = match updater.check_for_update().await? {
        Some(r) => r,
        None => {
            info!(version = env!("CARGO_PKG_VERSION"), "already up to date");
            return Ok(());
        }
    };

    info!(
        current = env!("CARGO_PKG_VERSION"),
        latest = %release.version,
        "new version available"
    );

    if !release.changelog.is_empty() {
        info!(changelog = %release.changelog);
    }

    if check_only {
        return Ok(());
    }

    // Step 2: Download dist archive.
    info!(version = %release.version, "downloading update...");
    let archive_path = updater.download_dist_archive(&release).await?;

    // Step 3: Extract dist archive.
    let dist_dir = updater::extract_dist_archive(&archive_path)?;

    // Step 4: Detect service manager and stop services.
    let service_manager = flatline::services::detect();
    if let Some(manager) = service_manager {
        info!(manager = ?manager, "stopping services");
        flatline::services::stop_services(manager).await?;
    } else {
        info!("no service manager detected, stopping wintermute via PID");
        updater.stop_wintermute_pid().await?;
    }

    // Step 5: Backup current binaries.
    updater.backup_binary("wintermute").await?;
    updater.backup_binary("flatline").await?;

    // Step 6: Install new binaries.
    let bin_dir = updater::install_dir()?;
    std::fs::create_dir_all(&bin_dir)
        .with_context(|| format!("failed to create {}", bin_dir.display()))?;

    for name in ["wintermute", "flatline"] {
        let src = dist_dir.join(name);
        let dest = bin_dir.join(name);
        anyhow::ensure!(
            src.exists(),
            "{name} binary not found in dist archive at {}",
            src.display()
        );
        tokio::fs::copy(&src, &dest)
            .await
            .with_context(|| format!("failed to install {name} binary"))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&dest, perms)
                .with_context(|| format!("failed to set {name} execute permission"))?;
        }
    }

    info!(dir = %bin_dir.display(), "binaries installed");

    // Step 7: Reinstall service files and start services.
    if let Some(manager) = service_manager {
        info!(manager = ?manager, "reinstalling service files");
        flatline::services::install_service_files(manager, &dist_dir).await?;

        info!(manager = ?manager, "starting services");
        flatline::services::start_services(manager).await?;
    } else {
        info!("no service manager detected; start wintermute and flatline manually");
    }

    info!(
        from = env!("CARGO_PKG_VERSION"),
        to = %release.version,
        "update complete"
    );

    Ok(())
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
