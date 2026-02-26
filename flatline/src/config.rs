//! Configuration loading for the Flatline supervisor.
//!
//! Loads `flatline.toml` with per-section defaults. All sections use
//! `#[serde(default)]` so a minimal or empty config file is valid.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

/// Top-level Flatline configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct FlatlineConfig {
    /// Model selection for LLM diagnosis calls.
    #[serde(default)]
    pub model: ModelConfig,

    /// Token budget limits for Flatline's own LLM usage.
    #[serde(default)]
    pub budget: FlatlineBudgetConfig,

    /// Periodic check timing.
    #[serde(default)]
    pub checks: ChecksConfig,

    /// Alert thresholds for various health metrics.
    #[serde(default)]
    pub thresholds: ThresholdsConfig,

    /// Automatic fix behavior toggles.
    #[serde(default)]
    pub auto_fix: AutoFixConfig,

    /// Reporting and notification settings.
    #[serde(default)]
    pub reports: ReportsConfig,

    /// Auto-update checking and application settings.
    #[serde(default)]
    pub update: UpdateConfig,

    /// Telegram notification targets.
    #[serde(default)]
    pub telegram: TelegramConfig,
}

/// Model selection for Flatline's LLM calls.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelConfig {
    /// Default model identifier (e.g. "ollama/qwen3:8b").
    #[serde(default = "default_model")]
    pub default: String,

    /// Optional fallback model when the default is unavailable.
    #[serde(default)]
    pub fallback: Option<String>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            default: default_model(),
            fallback: None,
        }
    }
}

/// Token budget for Flatline's own LLM usage.
#[derive(Debug, Clone, Deserialize)]
pub struct FlatlineBudgetConfig {
    /// Maximum tokens Flatline may consume per day.
    #[serde(default = "default_max_tokens_per_day")]
    pub max_tokens_per_day: u64,
}

impl Default for FlatlineBudgetConfig {
    fn default() -> Self {
        Self {
            max_tokens_per_day: default_max_tokens_per_day(),
        }
    }
}

/// Timing for periodic health checks.
#[derive(Debug, Clone, Deserialize)]
pub struct ChecksConfig {
    /// Seconds between periodic health check cycles.
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,

    /// Seconds after which health.json is considered stale.
    #[serde(default = "default_health_stale_threshold_secs")]
    pub health_stale_threshold_secs: u64,
}

impl Default for ChecksConfig {
    fn default() -> Self {
        Self {
            interval_secs: default_interval_secs(),
            health_stale_threshold_secs: default_health_stale_threshold_secs(),
        }
    }
}

/// Alert thresholds for health metrics.
#[derive(Debug, Clone, Deserialize)]
pub struct ThresholdsConfig {
    /// Tool failure rate above which an alert fires (0.0 - 1.0).
    #[serde(default = "default_tool_failure_rate")]
    pub tool_failure_rate: f64,

    /// Rolling window (hours) for tool failure rate calculation.
    #[serde(default = "default_tool_failure_window_hours")]
    pub tool_failure_window_hours: u64,

    /// Budget burn rate fraction that triggers an alert.
    #[serde(default = "default_budget_burn_rate_alert")]
    pub budget_burn_rate_alert: f64,

    /// Number of pending memories before alert.
    #[serde(default = "default_memory_pending_alert")]
    pub memory_pending_alert: u64,

    /// Days of tool inactivity before suggesting cleanup.
    #[serde(default = "default_unused_tool_days")]
    pub unused_tool_days: u64,

    /// Dynamic tool count above which a sprawl warning fires.
    #[serde(default = "default_max_tool_count_warning")]
    pub max_tool_count_warning: u64,

    /// Disk usage (GB) in ~/.wintermute above which a warning fires.
    #[serde(default = "default_disk_warning_gb")]
    pub disk_warning_gb: f64,
}

impl Default for ThresholdsConfig {
    fn default() -> Self {
        Self {
            tool_failure_rate: default_tool_failure_rate(),
            tool_failure_window_hours: default_tool_failure_window_hours(),
            budget_burn_rate_alert: default_budget_burn_rate_alert(),
            memory_pending_alert: default_memory_pending_alert(),
            unused_tool_days: default_unused_tool_days(),
            max_tool_count_warning: default_max_tool_count_warning(),
            disk_warning_gb: default_disk_warning_gb(),
        }
    }
}

/// Toggles for automatic fix behaviors.
#[derive(Debug, Clone, Deserialize)]
pub struct AutoFixConfig {
    /// Master switch for all automatic fixes.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Auto-restart Wintermute when crashed.
    #[serde(default = "default_true")]
    pub restart_on_crash: bool,

    /// Auto-quarantine tools exceeding the failure threshold.
    #[serde(default = "default_true")]
    pub quarantine_failing_tools: bool,

    /// Auto-disable scheduled tasks after consecutive failures.
    #[serde(default = "default_true")]
    pub disable_failing_tasks: bool,

    /// Auto-revert recent git changes correlated with failures.
    #[serde(default = "default_true")]
    pub revert_recent_changes: bool,

    /// Maximum automatic restarts allowed per hour before escalating.
    #[serde(default = "default_max_auto_restarts_per_hour")]
    pub max_auto_restarts_per_hour: u32,
}

impl Default for AutoFixConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            restart_on_crash: true,
            quarantine_failing_tools: true,
            disable_failing_tasks: true,
            revert_recent_changes: true,
            max_auto_restarts_per_hour: default_max_auto_restarts_per_hour(),
        }
    }
}

/// Reporting and notification timing.
#[derive(Debug, Clone, Deserialize)]
pub struct ReportsConfig {
    /// Time of day for the daily health report (HH:MM format).
    #[serde(default = "default_daily_health")]
    pub daily_health: String,

    /// Minutes to wait before repeating the same alert.
    #[serde(default = "default_alert_cooldown_mins")]
    pub alert_cooldown_mins: u64,

    /// Prefix prepended to all Telegram messages from Flatline.
    #[serde(default = "default_telegram_prefix")]
    pub telegram_prefix: String,
}

impl Default for ReportsConfig {
    fn default() -> Self {
        Self {
            daily_health: default_daily_health(),
            alert_cooldown_mins: default_alert_cooldown_mins(),
            telegram_prefix: default_telegram_prefix(),
        }
    }
}

/// Telegram notification targets.
#[derive(Debug, Clone, Deserialize)]
pub struct TelegramConfig {
    /// Environment variable name holding the bot token.
    #[serde(default = "default_bot_token_env")]
    pub bot_token_env: String,

    /// User IDs to receive Flatline notifications.
    #[serde(default)]
    pub notify_users: Vec<i64>,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            bot_token_env: default_bot_token_env(),
            notify_users: Vec::new(),
        }
    }
}

/// Auto-update checking and application settings.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateConfig {
    /// Master switch for update checking.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Update channel: "stable" or "nightly".
    #[serde(default = "default_channel")]
    pub channel: String,

    /// Time of day to check for updates (HH:MM, local time).
    #[serde(default = "default_check_time")]
    pub check_time: String,

    /// If true, apply updates without user confirmation.
    #[serde(default)]
    pub auto_apply: bool,

    /// Hours to wait for an idle window before nagging the user.
    #[serde(default = "default_idle_patience_hours")]
    pub idle_patience_hours: u64,

    /// Seconds to monitor health after applying an update.
    #[serde(default = "default_health_watch_secs")]
    pub health_watch_secs: u64,

    /// GitHub "owner/repo" for release checks.
    #[serde(default = "default_repo")]
    pub repo: String,

    /// If set, pin to this exact version and skip updates.
    #[serde(default)]
    pub pinned_version: Option<String>,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            channel: default_channel(),
            check_time: default_check_time(),
            auto_apply: false,
            idle_patience_hours: default_idle_patience_hours(),
            health_watch_secs: default_health_watch_secs(),
            repo: default_repo(),
            pinned_version: None,
        }
    }
}

/// Resolved filesystem paths for Flatline's own state.
#[derive(Debug, Clone)]
pub struct FlatlinePaths {
    /// Root directory for Flatline state (`~/.wintermute/flatline/`).
    pub root: PathBuf,

    /// Path to Flatline's SQLite state database.
    pub state_db: PathBuf,

    /// Directory for diagnosis report files.
    pub diagnoses_dir: PathBuf,

    /// Directory for proposed and applied fix patches.
    pub patches_dir: PathBuf,

    /// Directory for downloaded updates and rollback binaries.
    pub updates_dir: PathBuf,

    /// Subdirectory for pending (downloaded but not applied) updates.
    pub pending_dir: PathBuf,
}

impl FlatlineConfig {
    /// Validate that configuration values are within sane bounds.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.checks.interval_secs >= 10,
            "interval_secs must be >= 10"
        );
        anyhow::ensure!(
            (0.0..=1.0).contains(&self.thresholds.tool_failure_rate),
            "tool_failure_rate must be in [0.0, 1.0]"
        );
        anyhow::ensure!(
            (0.0..=1.0).contains(&self.thresholds.budget_burn_rate_alert),
            "budget_burn_rate_alert must be in [0.0, 1.0]"
        );
        anyhow::ensure!(
            self.thresholds.disk_warning_gb > 0.0,
            "disk_warning_gb must be positive"
        );
        anyhow::ensure!(
            self.auto_fix.max_auto_restarts_per_hour <= 20,
            "max_auto_restarts_per_hour must be <= 20"
        );
        anyhow::ensure!(
            self.update.channel == "stable" || self.update.channel == "nightly",
            "update.channel must be 'stable' or 'nightly'"
        );
        anyhow::ensure!(
            self.update.idle_patience_hours >= 1,
            "update.idle_patience_hours must be >= 1"
        );
        anyhow::ensure!(
            self.update.health_watch_secs >= 60,
            "update.health_watch_secs must be >= 60"
        );
        // Validate repo format to prevent URL/image injection.
        anyhow::ensure!(
            self.update.repo.contains('/')
                && self.update.repo.chars().all(|c| c.is_ascii_alphanumeric()
                    || c == '/'
                    || c == '.'
                    || c == '-'
                    || c == '_')
                && self.update.repo.split('/').count() == 2,
            "update.repo must be 'owner/repo' format"
        );
        // Validate check_time format (HH:MM).
        {
            let parts: Vec<&str> = self.update.check_time.split(':').collect();
            anyhow::ensure!(
                parts.len() == 2
                    && parts[0].len() == 2
                    && parts[1].len() == 2
                    && parts[0].parse::<u32>().is_ok_and(|h| h < 24)
                    && parts[1].parse::<u32>().is_ok_and(|m| m < 60),
                "update.check_time must be HH:MM format (00:00 - 23:59)"
            );
        }
        Ok(())
    }
}

/// Load Flatline configuration from a TOML file.
///
/// # Errors
///
/// Returns an error if the file cannot be read, parsed, or fails validation.
pub fn load_flatline_config(path: &Path) -> anyhow::Result<FlatlineConfig> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read flatline config at {}", path.display()))?;
    let config: FlatlineConfig = toml::from_str(&contents)
        .with_context(|| format!("failed to parse flatline config at {}", path.display()))?;
    config.validate()?;
    Ok(config)
}

/// Resolve Flatline's filesystem paths under `~/.wintermute/flatline/`.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
pub fn flatline_paths() -> anyhow::Result<FlatlinePaths> {
    let wintermute_root = wintermute::config::config_dir()?;
    let root = wintermute_root.join("flatline");
    let state_db = root.join("state.db");
    let diagnoses_dir = root.join("diagnoses");
    let patches_dir = root.join("patches");

    let updates_dir = root.join("updates");
    let pending_dir = updates_dir.join("pending");

    Ok(FlatlinePaths {
        root,
        state_db,
        diagnoses_dir,
        patches_dir,
        updates_dir,
        pending_dir,
    })
}

// Default value functions for serde.

fn default_model() -> String {
    "ollama/qwen3:8b".to_owned()
}

fn default_max_tokens_per_day() -> u64 {
    100_000
}

fn default_interval_secs() -> u64 {
    300
}

fn default_health_stale_threshold_secs() -> u64 {
    180
}

fn default_tool_failure_rate() -> f64 {
    0.5
}

fn default_tool_failure_window_hours() -> u64 {
    1
}

fn default_budget_burn_rate_alert() -> f64 {
    0.8
}

fn default_memory_pending_alert() -> u64 {
    100
}

fn default_unused_tool_days() -> u64 {
    30
}

fn default_max_tool_count_warning() -> u64 {
    40
}

fn default_disk_warning_gb() -> f64 {
    5.0
}

fn default_true() -> bool {
    true
}

fn default_max_auto_restarts_per_hour() -> u32 {
    3
}

fn default_daily_health() -> String {
    "08:00".to_owned()
}

fn default_alert_cooldown_mins() -> u64 {
    30
}

fn default_telegram_prefix() -> String {
    "\u{1fa7a} Flatline".to_owned()
}

fn default_bot_token_env() -> String {
    "WINTERMUTE_TELEGRAM_TOKEN".to_owned()
}

fn default_channel() -> String {
    "stable".to_owned()
}

fn default_check_time() -> String {
    "04:00".to_owned()
}

fn default_idle_patience_hours() -> u64 {
    6
}

fn default_health_watch_secs() -> u64 {
    300
}

fn default_repo() -> String {
    "pycckuu/wintermute".to_owned()
}
