//! Configuration loading and validation.
//!
//! Wintermute uses a split config model:
//! - `config.toml` — human-owned, agent can read but never write
//! - `agent.toml` — agent-owned, modifiable via execute_command

use std::path::{Path, PathBuf};

use serde::Deserialize;

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

/// Sandbox resource limits.
#[derive(Debug, Deserialize)]
pub struct SandboxConfig {
    /// Memory limit in megabytes.
    #[serde(default = "default_memory_mb")]
    pub memory_mb: u32,

    /// CPU core limit.
    #[serde(default = "default_cpu_cores")]
    pub cpu_cores: f64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            memory_mb: default_memory_mb(),
            cpu_cores: default_cpu_cores(),
        }
    }
}

/// Budget limits for token usage and tool calls.
#[derive(Debug, Deserialize)]
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
}

impl Default for EgressConfig {
    fn default() -> Self {
        Self {
            allowed_domains: Vec::new(),
            fetch_rate_limit: default_fetch_rate(),
            request_rate_limit: default_request_rate(),
        }
    }
}

// Default value functions for serde

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_budget_values() {
        let budget = BudgetConfig::default();
        assert_eq!(budget.max_tokens_per_session, 500_000);
        assert_eq!(budget.max_tokens_per_day, 5_000_000);
        assert_eq!(budget.max_tool_calls_per_turn, 20);
        assert_eq!(budget.max_dynamic_tools_per_turn, 20);
    }

    #[test]
    fn default_sandbox_values() {
        let sandbox = SandboxConfig::default();
        assert_eq!(sandbox.memory_mb, 2048);
        assert!((sandbox.cpu_cores - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn config_dir_resolves() {
        let dir = config_dir();
        assert!(dir.is_ok());
        let path = dir.expect("already checked");
        assert!(path.ends_with(".wintermute"));
    }

    #[test]
    fn parse_minimal_config() {
        let toml_str = r#"
[models]
default = "anthropic/claude-sonnet-4-5-20250929"

[channels.telegram]
bot_token_env = "WINTERMUTE_TELEGRAM_TOKEN"
allowed_users = [123456789]
"#;
        let config: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(
            config.models.default,
            "anthropic/claude-sonnet-4-5-20250929"
        );
        assert_eq!(config.channels.telegram.allowed_users, vec![123456789]);
    }
}
