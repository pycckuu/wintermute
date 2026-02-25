//! Configuration loading and validation.
//!
//! Wintermute uses a split config model:
//! - `config.toml` — human-owned, agent can read but never write
//! - `agent.toml` — agent-owned, modifiable via execute_command

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Top-level human-owned configuration.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Model routing configuration.
    pub models: ModelsConfig,

    /// Telegram channel configuration.
    pub channels: ChannelsConfig,

    /// Sandbox resource limits.
    #[serde(default)]
    pub sandbox: SandboxConfig,

    /// Token and cost budget limits.
    #[serde(default)]
    pub budget: BudgetConfig,

    /// Egress (outbound network) policy.
    #[serde(default)]
    pub egress: EgressConfig,

    /// Privacy boundary configuration.
    #[serde(default)]
    pub privacy: PrivacyConfig,
}

/// Top-level agent-owned configuration.
#[derive(Debug, Deserialize)]
pub struct AgentConfig {
    /// Agent personality settings.
    #[serde(default)]
    pub personality: PersonalityConfig,

    /// Heartbeat scheduler settings.
    #[serde(default)]
    pub heartbeat: HeartbeatConfig,

    /// Learning and promotion behavior settings.
    #[serde(default)]
    pub learning: LearningConfig,

    /// Scheduled built-in or dynamic tasks.
    #[serde(default)]
    pub scheduled_tasks: Vec<ScheduledTaskConfig>,

    /// Docker service definitions persisted by the agent.
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
}

/// Docker service definition persisted by the agent.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServiceConfig {
    /// Service name (e.g. "ollama").
    pub name: String,
    /// Docker image reference.
    pub image: String,
    /// Port mappings (host:container format).
    #[serde(default)]
    pub ports: Vec<String>,
    /// Volume mounts.
    #[serde(default)]
    pub volumes: Vec<String>,
    /// Restart policy.
    #[serde(default)]
    pub restart: Option<String>,
}

/// Model routing: default model, per-role and per-skill overrides.
#[derive(Debug, Deserialize)]
pub struct ModelsConfig {
    /// Default model identifier (e.g. "anthropic/claude-sonnet-4-5-20250929").
    pub default: String,

    /// Per-role model overrides (observer, embedding, etc.).
    #[serde(default)]
    pub roles: std::collections::HashMap<String, String>,

    /// Per-skill model overrides.
    #[serde(default)]
    pub skills: std::collections::HashMap<String, String>,
}

/// Channel configuration.
#[derive(Debug, Deserialize)]
pub struct ChannelsConfig {
    /// Telegram bot settings.
    pub telegram: TelegramConfig,
}

/// Telegram-specific configuration.
#[derive(Debug, Deserialize)]
pub struct TelegramConfig {
    /// Environment variable name holding the bot token.
    pub bot_token_env: String,

    /// Telegram user IDs allowed to interact with the agent.
    pub allowed_users: Vec<i64>,
}

/// Personality and identity settings for the agent.
#[derive(Debug, Deserialize)]
pub struct PersonalityConfig {
    /// Human-readable agent name.
    #[serde(default = "default_personality_name")]
    pub name: String,

    /// System prompt extension controlled by the user/agent config.
    #[serde(default)]
    pub soul: String,
}

impl Default for PersonalityConfig {
    fn default() -> Self {
        Self {
            name: default_personality_name(),
            soul: String::new(),
        }
    }
}

/// Sandbox resource limits.
#[derive(Debug, Clone, Deserialize)]
pub struct SandboxConfig {
    /// Memory limit in megabytes.
    #[serde(default = "default_memory_mb")]
    pub memory_mb: u32,

    /// CPU core limit.
    #[serde(default = "default_cpu_cores")]
    pub cpu_cores: f64,

    /// Optional container runtime override (e.g. `"runsc"` for gVisor).
    #[serde(default)]
    pub runtime: Option<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            memory_mb: default_memory_mb(),
            cpu_cores: default_cpu_cores(),
            runtime: None,
        }
    }
}

/// Budget limits for token usage and tool calls.
#[derive(Debug, Clone, Deserialize)]
pub struct BudgetConfig {
    /// Maximum tokens per agent session.
    #[serde(default = "default_session_tokens")]
    pub max_tokens_per_session: u64,

    /// Maximum tokens per day across all sessions.
    #[serde(default = "default_daily_tokens")]
    pub max_tokens_per_day: u64,

    /// Maximum tool calls per single LLM turn.
    #[serde(default = "default_tool_calls_per_turn")]
    pub max_tool_calls_per_turn: u32,

    /// Maximum dynamic tools included per LLM call.
    #[serde(default = "default_dynamic_tools_per_turn")]
    pub max_dynamic_tools_per_turn: u32,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_tokens_per_session: default_session_tokens(),
            max_tokens_per_day: default_daily_tokens(),
            max_tool_calls_per_turn: default_tool_calls_per_turn(),
            max_dynamic_tools_per_turn: default_dynamic_tools_per_turn(),
        }
    }
}

/// Egress (outbound network) policy configuration.
#[derive(Debug, Deserialize)]
pub struct EgressConfig {
    /// Domains pre-approved for outbound HTTP requests.
    #[serde(default)]
    pub allowed_domains: Vec<String>,

    /// Rate limit for web_fetch (GET) calls per minute.
    #[serde(default = "default_fetch_rate")]
    pub fetch_rate_limit: u32,

    /// Rate limit for web_request (POST/PUT/DELETE) calls per minute.
    #[serde(default = "default_request_rate")]
    pub request_rate_limit: u32,

    /// Rate limit for browser actions per minute.
    #[serde(default = "default_browser_rate")]
    pub browser_rate_limit: u32,

    /// Maximum file download size in megabytes for web_fetch save_to mode.
    #[serde(default = "default_max_file_download_mb")]
    pub max_file_download_mb: u32,
}

/// Privacy boundary policy configuration.
#[derive(Debug, Deserialize, Default)]
pub struct PrivacyConfig {
    /// Domains that always require explicit user approval.
    #[serde(default)]
    pub always_approve_domains: Vec<String>,

    /// Domains blocked entirely for outbound requests.
    #[serde(default)]
    pub blocked_domains: Vec<String>,
}

impl Default for EgressConfig {
    fn default() -> Self {
        Self {
            allowed_domains: Vec::new(),
            fetch_rate_limit: default_fetch_rate(),
            request_rate_limit: default_request_rate(),
            browser_rate_limit: default_browser_rate(),
            max_file_download_mb: default_max_file_download_mb(),
        }
    }
}

/// Heartbeat scheduler settings.
#[derive(Debug, Deserialize)]
pub struct HeartbeatConfig {
    /// Enables or disables heartbeat processing.
    #[serde(default = "default_heartbeat_enabled")]
    pub enabled: bool,

    /// Tick interval in seconds.
    #[serde(default = "default_heartbeat_interval_secs")]
    pub interval_secs: u64,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            enabled: default_heartbeat_enabled(),
            interval_secs: default_heartbeat_interval_secs(),
        }
    }
}

/// Promotion mode for observer extractions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromotionMode {
    /// Auto-promote after threshold confirmations.
    #[default]
    Auto,
    /// Suggest via Telegram, user approves.
    Suggest,
    /// No automatic promotion.
    Off,
}

/// Learning and promotion settings.
#[derive(Debug, Clone, Deserialize)]
pub struct LearningConfig {
    /// Enables or disables observer-driven learning.
    #[serde(default = "default_learning_enabled")]
    pub enabled: bool,

    /// Promotion mode for pending observations.
    #[serde(default)]
    pub promotion_mode: PromotionMode,

    /// Auto-promotion threshold for repeated confirmations.
    #[serde(default = "default_auto_promote_threshold")]
    pub auto_promote_threshold: u32,
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            enabled: default_learning_enabled(),
            promotion_mode: PromotionMode::default(),
            auto_promote_threshold: default_auto_promote_threshold(),
        }
    }
}

/// Agent-owned scheduled task configuration.
#[derive(Debug, Deserialize)]
pub struct ScheduledTaskConfig {
    /// Task name used for identification and logging.
    pub name: String,

    /// Cron expression defining execution cadence.
    pub cron: String,

    /// Optional built-in task name.
    #[serde(default)]
    pub builtin: Option<String>,

    /// Optional dynamic tool name.
    #[serde(default)]
    pub tool: Option<String>,

    /// Optional task-specific budget token limit.
    #[serde(default)]
    pub budget_tokens: Option<u64>,

    /// Whether to notify user on completion.
    #[serde(default)]
    pub notify: bool,

    /// Whether this task is active.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Resolved runtime paths under `~/.wintermute`.
#[derive(Debug, Clone)]
pub struct RuntimePaths {
    /// Runtime directory (`~/.wintermute`).
    pub root: PathBuf,
    /// Human-owned config file path.
    pub config_toml: PathBuf,
    /// Agent-owned config file path.
    pub agent_toml: PathBuf,
    /// Runtime env file path.
    pub env_file: PathBuf,
    /// Scripts directory path.
    pub scripts_dir: PathBuf,
    /// Workspace directory path.
    pub workspace_dir: PathBuf,
    /// Data directory path.
    pub data_dir: PathBuf,
    /// Backups directory path.
    pub backups_dir: PathBuf,
    /// Memory database path.
    pub memory_db: PathBuf,
    /// PID file path for process monitoring.
    pub pid_file: PathBuf,
    /// Health JSON file path.
    pub health_json: PathBuf,
    /// System Identity Document path (generated by heartbeat).
    pub identity_md: PathBuf,
    /// User profile document path (generated by weekly digest).
    pub user_md: PathBuf,
}

// Default value functions for serde

fn default_personality_name() -> String {
    "Wintermute".to_owned()
}
fn default_memory_mb() -> u32 {
    2048
}
fn default_cpu_cores() -> f64 {
    2.0
}
fn default_session_tokens() -> u64 {
    500_000
}
fn default_daily_tokens() -> u64 {
    5_000_000
}
fn default_tool_calls_per_turn() -> u32 {
    20
}
fn default_dynamic_tools_per_turn() -> u32 {
    20
}
fn default_fetch_rate() -> u32 {
    30
}
fn default_request_rate() -> u32 {
    10
}
fn default_browser_rate() -> u32 {
    60
}
fn default_max_file_download_mb() -> u32 {
    500
}
fn default_heartbeat_enabled() -> bool {
    true
}
fn default_heartbeat_interval_secs() -> u64 {
    60
}
fn default_learning_enabled() -> bool {
    true
}
fn default_auto_promote_threshold() -> u32 {
    3
}
fn default_true() -> bool {
    true
}

/// Load the human-owned config from a TOML file.
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed.
pub fn load_config(path: &Path) -> anyhow::Result<Config> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read config at {}: {e}", path.display()))?;
    let config: Config = toml::from_str(&contents)
        .map_err(|e| anyhow::anyhow!("failed to parse config at {}: {e}", path.display()))?;
    Ok(config)
}

/// Load the agent-owned config from a TOML file.
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed.
pub fn load_agent_config(path: &Path) -> anyhow::Result<AgentConfig> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read agent config at {}: {e}", path.display()))?;
    let config: AgentConfig = toml::from_str(&contents)
        .map_err(|e| anyhow::anyhow!("failed to parse agent config at {}: {e}", path.display()))?;
    Ok(config)
}

/// Resolve the default config directory (`~/.wintermute/`).
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
pub fn config_dir() -> anyhow::Result<PathBuf> {
    let home = directories::BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    Ok(home.home_dir().join(".wintermute"))
}

/// Resolve runtime paths under `~/.wintermute`.
///
/// # Errors
///
/// Returns an error when the base config directory cannot be determined.
pub fn runtime_paths() -> anyhow::Result<RuntimePaths> {
    let root = config_dir()?;
    let config_toml = root.join("config.toml");
    let scripts_dir = root.join("scripts");
    let agent_toml = root.join("agent.toml");
    let env_file = root.join(".env");
    let workspace_dir = root.join("workspace");
    let data_dir = root.join("data");
    let backups_dir = root.join("backups");
    let memory_db = data_dir.join("memory.db");
    let pid_file = root.join("wintermute.pid");
    let health_json = root.join("health.json");
    let identity_md = root.join("IDENTITY.md");
    let user_md = root.join("USER.md");

    Ok(RuntimePaths {
        root,
        config_toml,
        agent_toml,
        env_file,
        scripts_dir,
        workspace_dir,
        data_dir,
        backups_dir,
        memory_db,
        pid_file,
        health_json,
        identity_md,
        user_md,
    })
}

/// Load the default human-owned config from `~/.wintermute/config.toml`.
///
/// # Errors
///
/// Returns an error if paths cannot be resolved or config parsing fails.
pub fn load_default_config() -> anyhow::Result<Config> {
    let paths = runtime_paths()?;
    load_config(&paths.config_toml)
}

/// Load the default agent-owned config from `~/.wintermute/agent.toml`.
///
/// # Errors
///
/// Returns an error if paths cannot be resolved or config parsing fails.
pub fn load_default_agent_config() -> anyhow::Result<AgentConfig> {
    let paths = runtime_paths()?;
    load_agent_config(&paths.agent_toml)
}

/// Return all provider model specs declared in config in deterministic order.
pub fn all_model_specs(models: &ModelsConfig) -> Vec<String> {
    let mut ordered = Vec::new();
    ordered.push(models.default.clone());

    let mut role_specs: Vec<_> = models.roles.iter().collect();
    role_specs.sort_by_key(|(k, _)| *k);
    for (_, spec) in role_specs {
        ordered.push(spec.clone());
    }

    let mut skill_specs: Vec<_> = models.skills.iter().collect();
    skill_specs.sort_by_key(|(k, _)| *k);
    for (_, spec) in skill_specs {
        ordered.push(spec.clone());
    }

    let mut seen = HashMap::new();
    ordered
        .into_iter()
        .filter(|spec| seen.insert(spec.clone(), true).is_none())
        .collect()
}
