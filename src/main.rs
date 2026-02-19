//! Wintermute CLI entry point.
//!
//! Provides subcommands for initializing, starting, and managing
//! the Wintermute agent.

use std::fs;
use std::path::Path;

use anyhow::Context;
use chrono::Utc;
use clap::{Parser, Subcommand};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::Connection;
use tracing::{info, warn};

use wintermute::config::{
    load_default_agent_config, load_default_config, runtime_paths, RuntimePaths,
};
use wintermute::credentials::{
    enforce_private_file_permissions, load_default_credentials, Credentials,
};
use wintermute::executor::docker::DockerExecutor;
use wintermute::executor::redactor::Redactor;
use wintermute::executor::Executor;
use wintermute::logging;
use wintermute::providers::router::ModelRouter;

const BOOTSTRAP_MIGRATION: &str = "001_schema.sql";

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

async fn handle_start() -> anyhow::Result<()> {
    let paths = runtime_paths()?;
    let config = load_default_config()
        .with_context(|| format!("failed to load {}", paths.config_toml.display()))?;
    let _agent_config = load_default_agent_config()
        .with_context(|| format!("failed to load {}", paths.agent_toml.display()))?;
    let credentials = load_default_credentials()
        .with_context(|| format!("failed to load {}", paths.env_file.display()))?;
    let token_key = &config.channels.telegram.bot_token_env;
    let _telegram_token = credentials.require(token_key)?;

    let router = ModelRouter::from_config(&config.models, &credentials)?;
    if !router.has_model(&config.models.default) {
        return Err(anyhow::anyhow!(
            "default model '{}' is not available",
            config.models.default
        ));
    }

    ensure_docker_available_for_start(DockerExecutor::docker_available().await)?;

    let redactor = Redactor::new(credentials.known_secrets());
    let executor = DockerExecutor::new(&config, &paths, redactor).await?;
    let health = executor.health_check().await?;
    if !health.is_healthy() {
        return Err(anyhow::anyhow!("docker executor unhealthy: {health:?}"));
    }

    info!(
        default_model = %config.models.default,
        provider_count = router.provider_count(),
        "startup checks complete; core loop wiring will be added in phase 2"
    );
    Ok(())
}

fn ensure_docker_available_for_start(is_available: bool) -> anyhow::Result<()> {
    if !is_available {
        return Err(anyhow::anyhow!(
            "docker is required in strict mode; install/start Docker and retry"
        ));
    }
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
    sqlx::query(script)
        .execute(&mut connection)
        .await
        .context("failed to apply bootstrap migration")?;

    sqlx::query("INSERT OR IGNORE INTO migrations(name) VALUES (?1)")
        .bind(BOOTSTRAP_MIGRATION)
        .execute(&mut connection)
        .await
        .context("failed to persist migration marker")?;

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
cron = "0 3 * * *"
builtin = "backup"
"#
}

fn default_env_file() -> &'static str {
    "WINTERMUTE_TELEGRAM_TOKEN=\nANTHROPIC_API_KEY=\n"
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
