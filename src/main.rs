//! Wintermute CLI entry point.
//!
//! Provides subcommands for initializing, starting, and managing
//! the Wintermute agent.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use chrono::Utc;
use clap::{Parser, Subcommand};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::Connection;
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use wintermute::agent::approval::ApprovalManager;
use wintermute::agent::budget::DailyBudget;
use wintermute::agent::policy::{PolicyContext, RateLimiter};
use wintermute::agent::{SessionRouter, TelegramOutbound};
use wintermute::config::{
    load_default_agent_config, load_default_config, runtime_paths, RuntimePaths,
};
use wintermute::credentials::{
    enforce_private_file_permissions, load_default_credentials, resolve_anthropic_auth,
    AnthropicAuth, Credentials,
};
use wintermute::executor::direct::DirectExecutor;
use wintermute::executor::docker::DockerExecutor;
use wintermute::executor::redactor::Redactor;
use wintermute::executor::Executor;
use wintermute::logging;
use wintermute::memory::{MemoryEngine, TrustSource};
use wintermute::providers::router::ModelRouter;
use wintermute::telegram;
use wintermute::tools::registry::DynamicToolRegistry;
use wintermute::tools::ToolRouter;

const BOOTSTRAP_MIGRATION: &str = "001_schema.sql";
const MEMORY_MIGRATION: &str = "002_memory.sql";

/// Wintermute — a self-coding AI agent.
#[derive(Parser)]
#[command(name = "wintermute", version, about)]
struct Cli {
    /// Subcommand to execute
    #[command(subcommand)]
    command: Command,
}

/// Available CLI subcommands.
#[derive(Subcommand)]
enum Command {
    /// First-time setup: create config files, initialize database, build sandbox
    Init,
    /// Start the agent (connects to Telegram, begins listening)
    Start,
    /// Show health status, sandbox info, and memory stats
    Status,
    /// Recreate the sandbox container and reinstall dependencies
    Reset,
    /// Trigger an immediate backup of scripts and memory
    Backup {
        /// Subcommand for backup operations
        #[command(subcommand)]
        action: Option<BackupAction>,
    },
}

/// Backup subcommands.
#[derive(Subcommand)]
enum BackupAction {
    /// List available backups
    List,
    /// Restore a specific backup by index
    Restore {
        /// Backup index to restore
        index: u32,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Start subcommand gets production logging (JSON + file rotation).
    // All other subcommands get simple CLI logging (stderr only).
    let _logging_guard = match &cli.command {
        Command::Start => {
            let paths = runtime_paths()?;
            let logs_dir = paths.data_dir.join("logs");
            Some(logging::init_production(&logs_dir)?)
        }
        _ => {
            logging::init_cli();
            None
        }
    };

    match cli.command {
        Command::Init => handle_init().await?,
        Command::Start => handle_start().await?,
        Command::Status => handle_status().await?,
        Command::Reset => handle_reset().await?,
        Command::Backup { action } => match action {
            None => handle_backup(None).await?,
            Some(BackupAction::List) => handle_backup(Some(BackupRequest::List)).await?,
            Some(BackupAction::Restore { index }) => {
                handle_backup(Some(BackupRequest::Restore { index })).await?
            }
        },
    }

    Ok(())
}

enum BackupRequest {
    List,
    Restore { index: u32 },
}

async fn handle_init() -> anyhow::Result<()> {
    let paths = runtime_paths()?;
    ensure_runtime_layout(&paths)?;
    write_default_files(&paths)?;
    apply_bootstrap_migration(&paths).await?;

    if DockerExecutor::docker_available().await {
        info!("docker daemon detected; start command will use DockerExecutor");
    } else {
        warn!("docker daemon not detected; start command will fail in strict Docker mode");
    }

    info!(root = %paths.root.display(), "wintermute init complete");
    Ok(())
}

/// Outbound channel buffer size.
const OUTBOUND_CHANNEL_CAPACITY: usize = 256;

async fn handle_start() -> anyhow::Result<()> {
    let paths = runtime_paths()?;
    let config = load_default_config()
        .with_context(|| format!("failed to load {}", paths.config_toml.display()))?;
    let agent_config = load_default_agent_config()
        .with_context(|| format!("failed to load {}", paths.agent_toml.display()))?;
    let credentials = load_default_credentials()
        .with_context(|| format!("failed to load {}", paths.env_file.display()))?;
    let token_key = &config.channels.telegram.bot_token_env;
    let telegram_token = credentials.require(token_key)?;

    // Resolve auth once so the router and redactor use the same token.
    let mut all_secrets = credentials.known_secrets();
    if let Some(auth) = resolve_anthropic_auth(&credentials) {
        match &auth {
            AnthropicAuth::OAuth { .. } => info!("anthropic auth resolved: OAuth token"),
            AnthropicAuth::ApiKey(_) => info!("anthropic auth resolved: API key"),
        }
        all_secrets.extend(auth.secret_values());
    } else {
        warn!("no Anthropic credentials found; provider will be unavailable");
    }

    let router = ModelRouter::from_config(&config.models, &credentials)?;
    if !router.has_model(&config.models.default) {
        return Err(anyhow::anyhow!(
            "default model '{}' is not available",
            config.models.default
        ));
    }

    // Set up executor: Docker preferred, Direct as fallback
    let redactor = Redactor::new(all_secrets.clone());
    let executor: Arc<dyn Executor> = if DockerExecutor::docker_available().await {
        let docker = DockerExecutor::new(&config, &paths, redactor.clone()).await?;
        let health = docker.health_check().await?;
        if !health.is_healthy() {
            return Err(anyhow::anyhow!("docker executor unhealthy: {health:?}"));
        }
        Arc::new(docker)
    } else {
        warn!("docker unavailable; using direct executor (maintenance-only)");
        Arc::new(DirectExecutor::new(
            paths.scripts_dir.clone(),
            paths.workspace_dir.clone(),
        ))
    };

    // Create connection pool and run migrations for the memory engine.
    // WAL mode enables concurrent reads while the writer actor holds a write lock.
    // trusted_schema=OFF prevents untrusted SQL functions in schema definitions.
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&paths.memory_db)
                .create_if_missing(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .pragma("trusted_schema", "OFF")
                .pragma("foreign_keys", "ON"),
        )
        .await
        .context("failed to create sqlite pool for memory engine")?;
    apply_memory_migration(&pool).await?;

    let memory = Arc::new(
        MemoryEngine::new(pool, None)
            .await
            .context("failed to initialise memory engine")?,
    );

    // Seed trust ledger with pre-approved domains from config.
    for domain in &config.egress.allowed_domains {
        memory
            .trust_domain(domain, TrustSource::Config)
            .await
            .context("failed to seed trust ledger")?;
    }

    // Phase 2 wiring
    let daily_budget = Arc::new(DailyBudget::new(config.budget.max_tokens_per_day));
    let approval_manager = Arc::new(ApprovalManager::new());

    let registry = DynamicToolRegistry::new(paths.scripts_dir.clone())
        .context("failed to create tool registry")?;

    let (telegram_tx, telegram_rx) = mpsc::channel::<TelegramOutbound>(OUTBOUND_CHANNEL_CAPACITY);

    let fetch_limiter = Arc::new(RateLimiter::new(60, config.egress.fetch_rate_limit));
    let request_limiter = Arc::new(RateLimiter::new(60, config.egress.request_rate_limit));
    let browser_limiter = Arc::new(RateLimiter::new(60, config.egress.browser_rate_limit));

    let policy_context = PolicyContext {
        allowed_domains: config.egress.allowed_domains.clone(),
        blocked_domains: config.privacy.blocked_domains.clone(),
        always_approve_domains: config.privacy.always_approve_domains.clone(),
        executor_kind: executor.kind(),
    };

    let observer_redactor = redactor.clone();
    let tool_router = Arc::new(ToolRouter::new(
        Arc::clone(&executor),
        redactor,
        Arc::clone(&memory),
        Arc::clone(&registry),
        Some(telegram_tx.clone()),
        fetch_limiter,
        request_limiter,
        browser_limiter,
        None, // No browser bridge configured; tool returns unavailable when called
    ));

    let config_arc = Arc::new(config);
    let agent_config_arc = Arc::new(agent_config);
    let router_arc = Arc::new(router);

    // Phase 3: Observer channel + background task
    let observer_tx = if agent_config_arc.learning.enabled {
        let (tx, rx) = mpsc::channel::<wintermute::observer::ObserverEvent>(64);
        let observer_deps = wintermute::observer::ObserverDeps {
            memory: Arc::clone(&memory),
            router: Arc::clone(&router_arc),
            daily_budget: Arc::clone(&daily_budget),
            redactor: observer_redactor,
            learning_config: agent_config_arc.learning.clone(),
            telegram_tx: telegram_tx.clone(),
        };
        tokio::spawn(wintermute::observer::run_observer(observer_deps, rx));
        info!("observer pipeline spawned");
        Some(tx)
    } else {
        info!("observer disabled via learning.enabled = false");
        None
    };

    let session_router = Arc::new(SessionRouter::new(
        Arc::clone(&router_arc),
        Arc::clone(&tool_router),
        Arc::clone(&memory),
        Arc::clone(&daily_budget),
        Arc::clone(&approval_manager),
        policy_context,
        telegram_tx.clone(),
        Arc::clone(&config_arc),
        Arc::clone(&agent_config_arc),
        observer_tx,
    ));

    // Phase 3: Heartbeat background task with graceful shutdown via Ctrl+C.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("ctrl-c received, signalling heartbeat shutdown");
            let _ = shutdown_tx.send(true);
        }
    });
    if agent_config_arc.heartbeat.enabled {
        let notify_user_id = match config_arc.channels.telegram.allowed_users.first() {
            Some(&id) => id,
            None => {
                warn!("no allowed_users configured; heartbeat notifications disabled");
                0
            }
        };
        let heartbeat_deps = wintermute::heartbeat::HeartbeatDeps {
            config: Arc::clone(&config_arc),
            agent_config: Arc::clone(&agent_config_arc),
            memory: Arc::clone(&memory),
            executor: Arc::clone(&executor),
            tool_router: Arc::clone(&tool_router),
            router: Arc::clone(&router_arc),
            daily_budget: Arc::clone(&daily_budget),
            telegram_tx: telegram_tx.clone(),
            notify_user_id,
            paths: paths.clone(),
            session_router: Arc::clone(&session_router),
        };
        tokio::spawn(wintermute::heartbeat::run_heartbeat(
            heartbeat_deps,
            Instant::now(),
            shutdown_rx,
        ));
        info!("heartbeat spawned");
    } else {
        info!("heartbeat disabled via heartbeat.enabled = false");
    }

    info!(
        default_model = %config_arc.models.default,
        "starting telegram bot"
    );

    telegram::run_telegram(
        &telegram_token,
        config_arc,
        session_router,
        approval_manager,
        telegram_rx,
        all_secrets,
        executor,
        memory,
        registry,
        paths,
    )
    .await?;

    Ok(())
}

async fn handle_status() -> anyhow::Result<()> {
    let paths = runtime_paths()?;
    let initialized =
        paths.config_toml.exists() && paths.agent_toml.exists() && paths.env_file.exists();
    info!(
        initialized,
        config_exists = paths.config_toml.exists(),
        agent_exists = paths.agent_toml.exists(),
        env_exists = paths.env_file.exists(),
        memory_exists = paths.memory_db.exists(),
        "runtime status"
    );

    let docker = DockerExecutor::docker_available().await;
    if docker {
        info!("docker is available");
    } else {
        warn!("docker is unavailable");
    }

    if initialized {
        let config = load_default_config()?;
        let credentials = credentials_or_default();
        let router = ModelRouter::from_config(&config.models, &credentials);
        match router {
            Ok(router) => {
                info!(
                    default_model = %config.models.default,
                    providers = router.available_specs().len(),
                    "model router status"
                );
            }
            Err(err) => warn!(error = %err, "model router not ready"),
        }
    }

    Ok(())
}

async fn handle_reset() -> anyhow::Result<()> {
    let paths = runtime_paths()?;
    let config = load_default_config()
        .with_context(|| format!("failed to load {}", paths.config_toml.display()))?;
    ensure_runtime_layout(&paths)?;
    apply_bootstrap_migration(&paths).await?;

    if !DockerExecutor::docker_available().await {
        warn!("docker unavailable; skipped sandbox reset");
        return Ok(());
    }

    let credentials = credentials_or_default();
    let redactor = Redactor::new(credentials.known_secrets());
    let executor = DockerExecutor::new(&config, &paths, redactor).await?;
    executor.reset_container(&config).await?;

    info!("sandbox reset complete");
    Ok(())
}

async fn handle_backup(request: Option<BackupRequest>) -> anyhow::Result<()> {
    let paths = runtime_paths()?;
    ensure_runtime_layout(&paths)?;

    match request {
        None => {
            let stamp = Utc::now().format("%Y%m%d-%H%M%S").to_string();
            let destination = paths.backups_dir.join(stamp);
            fs::create_dir_all(&destination)?;

            if paths.memory_db.exists() {
                fs::copy(&paths.memory_db, destination.join("memory.db")).with_context(|| {
                    format!(
                        "failed to copy memory db from {}",
                        paths.memory_db.display()
                    )
                })?;
            }

            if paths.scripts_dir.exists() {
                copy_directory_recursive(&paths.scripts_dir, &destination.join("scripts"))?;
            }

            info!(backup_dir = %destination.display(), "backup created");
        }
        Some(BackupRequest::List) => {
            let backups = list_backups(&paths.backups_dir)?;
            if backups.is_empty() {
                info!("no backups available");
            } else {
                for (idx, backup) in backups.iter().enumerate() {
                    let display_index = idx.saturating_add(1);
                    info!(index = display_index, path = %backup.display(), "backup");
                }
            }
        }
        Some(BackupRequest::Restore { index }) => {
            let backups = list_backups(&paths.backups_dir)?;
            if backups.is_empty() {
                return Err(anyhow::anyhow!("no backups available"));
            }

            let one_based = usize::try_from(index)
                .map_err(|_| anyhow::anyhow!("backup index does not fit into usize"))?;
            if one_based == 0 || one_based > backups.len() {
                return Err(anyhow::anyhow!(
                    "backup index out of range: {index} (valid 1..={})",
                    backups.len()
                ));
            }
            let zero_based = one_based
                .checked_sub(1)
                .ok_or_else(|| anyhow::anyhow!("backup index cannot be zero"))?;
            let selected = backups.get(zero_based).ok_or_else(|| {
                anyhow::anyhow!(
                    "backup index out of range: {index} (valid 1..={})",
                    backups.len()
                )
            })?;

            let backup_db = selected.join("memory.db");
            if backup_db.exists() {
                fs::copy(&backup_db, &paths.memory_db).with_context(|| {
                    format!("failed to restore memory db from {}", backup_db.display())
                })?;
            }

            let backup_scripts = selected.join("scripts");
            if backup_scripts.exists() {
                if paths.scripts_dir.exists() {
                    fs::remove_dir_all(&paths.scripts_dir).with_context(|| {
                        format!(
                            "failed to remove scripts dir {}",
                            paths.scripts_dir.display()
                        )
                    })?;
                }
                copy_directory_recursive(&backup_scripts, &paths.scripts_dir)?;
            }

            info!(backup = %selected.display(), "backup restored");
        }
    }

    Ok(())
}

fn credentials_or_default() -> Credentials {
    load_default_credentials().unwrap_or_else(|_| Credentials::default())
}

fn ensure_runtime_layout(paths: &RuntimePaths) -> anyhow::Result<()> {
    fs::create_dir_all(&paths.root)?;
    fs::create_dir_all(&paths.scripts_dir)?;
    fs::create_dir_all(&paths.workspace_dir)?;
    fs::create_dir_all(&paths.data_dir)?;
    fs::create_dir_all(&paths.backups_dir)?;
    Ok(())
}

fn write_default_files(paths: &RuntimePaths) -> anyhow::Result<()> {
    write_if_missing(&paths.config_toml, include_str!("../config.example.toml"))?;
    write_if_missing(&paths.agent_toml, default_agent_toml())?;
    write_if_missing(&paths.env_file, default_env_file())?;
    enforce_private_file_permissions(&paths.env_file)?;
    write_if_missing(
        &paths.scripts_dir.join(".gitignore"),
        "__pycache__/\n*.pyc\n*.pyo\n",
    )?;
    Ok(())
}

async fn apply_bootstrap_migration(paths: &RuntimePaths) -> anyhow::Result<()> {
    let options = SqliteConnectOptions::new()
        .filename(&paths.memory_db)
        .create_if_missing(true);
    let mut connection = sqlx::SqliteConnection::connect_with(&options)
        .await
        .with_context(|| format!("failed to connect sqlite at {}", paths.memory_db.display()))?;

    let script = include_str!("../migrations/001_schema.sql");
    sqlx::raw_sql(script)
        .execute(&mut connection)
        .await
        .context("failed to apply bootstrap migration")?;

    sqlx::query("INSERT OR IGNORE INTO migrations(name) VALUES (?1)")
        .bind(BOOTSTRAP_MIGRATION)
        .execute(&mut connection)
        .await
        .context("failed to persist migration marker")?;

    // Apply memory migration (002) if not yet applied.
    let applied: Option<(String,)> = sqlx::query_as("SELECT name FROM migrations WHERE name = ?1")
        .bind(MEMORY_MIGRATION)
        .fetch_optional(&mut connection)
        .await
        .context("failed to check memory migration")?;

    if applied.is_none() {
        let memory_script = include_str!("../migrations/002_memory.sql");
        sqlx::raw_sql(memory_script)
            .execute(&mut connection)
            .await
            .context("failed to apply memory migration")?;

        sqlx::query("INSERT OR IGNORE INTO migrations(name) VALUES (?1)")
            .bind(MEMORY_MIGRATION)
            .execute(&mut connection)
            .await
            .context("failed to persist memory migration marker")?;
    }

    Ok(())
}

/// Apply memory migration to an existing pool (for startup after init).
async fn apply_memory_migration(pool: &sqlx::SqlitePool) -> anyhow::Result<()> {
    let applied: Option<(String,)> = sqlx::query_as("SELECT name FROM migrations WHERE name = ?1")
        .bind(MEMORY_MIGRATION)
        .fetch_optional(pool)
        .await
        .context("failed to check memory migration")?;

    if applied.is_none() {
        let memory_script = include_str!("../migrations/002_memory.sql");
        sqlx::raw_sql(memory_script)
            .execute(pool)
            .await
            .context("failed to apply memory migration")?;

        sqlx::query("INSERT OR IGNORE INTO migrations(name) VALUES (?1)")
            .bind(MEMORY_MIGRATION)
            .execute(pool)
            .await
            .context("failed to persist memory migration marker")?;
    }

    Ok(())
}

fn write_if_missing(path: &Path, content: &str) -> anyhow::Result<()> {
    if !path.exists() {
        fs::write(path, content)
            .with_context(|| format!("failed writing file {}", path.display()))?;
    }
    Ok(())
}

fn default_agent_toml() -> &'static str {
    r#"[personality]
name = "Wintermute"
soul = """
You are a personal AI agent. Competent, direct, proactive.
You solve problems by writing code, testing it, and iterating.
When you solve something reusable, save it as a tool with create_tool.
"""

[heartbeat]
enabled = true
interval_secs = 60

[learning]
enabled = true
promotion_mode = "auto"
auto_promote_threshold = 3

[[scheduled_tasks]]
name = "daily_backup"
cron = "0 0 3 * * *"
builtin = "backup"
"#
}

fn default_env_file() -> &'static str {
    "WINTERMUTE_TELEGRAM_TOKEN=\nANTHROPIC_API_KEY=\n# ANTHROPIC_OAUTH_TOKEN=\n"
}

fn list_backups(backups_dir: &Path) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let mut entries = Vec::new();
    if !backups_dir.exists() {
        return Ok(entries);
    }
    for entry in fs::read_dir(backups_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            entries.push(path);
        }
    }
    entries.sort();
    entries.reverse();
    Ok(entries)
}

fn copy_directory_recursive(source: &Path, destination: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let src = entry.path();
        let dst = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_directory_recursive(&src, &dst)?;
        } else if file_type.is_file() {
            fs::copy(&src, &dst).with_context(|| {
                format!("failed to copy file {} to {}", src.display(), dst.display())
            })?;
        }
    }
    Ok(())
}
