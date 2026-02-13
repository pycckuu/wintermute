//! Configuration loading and management (spec 18).
//!
//! Loads PFAR configuration from `./config.toml` (or `$PFAR_CONFIG_PATH`).
//! Environment variables override file values; file values override defaults.
//!
//! Precedence: env vars > config file > defaults.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

// ── Top-level config ────────────────────────────────────────────

/// Top-level PFAR configuration loaded from TOML (spec 18.1).
///
/// Path: `./config.toml` or `$PFAR_CONFIG_PATH`.
/// Env vars override file values; file values override defaults.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PfarConfig {
    /// Kernel core settings (spec 18.1 `[kernel]`).
    pub kernel: KernelConfig,
    /// Filesystem paths for persistent state (spec 18.1).
    pub paths: PathsConfig,
    /// LLM provider configuration (spec 18.1 `[llm]`).
    pub llm: LlmConfig,
    /// Adapter configuration (spec 18.1 `[adapter]`).
    pub adapter: AdapterConfig,
}

impl PfarConfig {
    /// Load configuration with precedence: env vars > TOML file > defaults (spec 18.1).
    ///
    /// Config file path: `$PFAR_CONFIG_PATH` or `./config.toml`.
    /// If the file does not exist, returns defaults (backward compatible).
    pub fn load() -> Result<Self> {
        let mut config = Self::load_from_file()?;
        config.apply_overrides(|key| std::env::var(key).ok());
        Ok(config)
    }

    /// Load from TOML file only, no env overrides (spec 18.1).
    fn load_from_file() -> Result<Self> {
        let path = Self::config_path()?;
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                tracing::info!(path = %path.display(), "loading config from file");
                let config: PfarConfig =
                    toml::from_str(&contents).context("failed to parse config TOML")?;
                Ok(config)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!("no config file found, using defaults");
                Ok(PfarConfig::default())
            }
            Err(e) => Err(anyhow::anyhow!("failed to read config file: {e}")),
        }
    }

    /// Resolve config file path (spec 18.1).
    ///
    /// Checks `$PFAR_CONFIG_PATH` first, then `./config.toml` in the working directory.
    fn config_path() -> Result<PathBuf> {
        Self::config_path_with(|key| std::env::var(key).ok())
    }

    /// Resolve config path using a custom env resolver (for testing).
    fn config_path_with(env: impl Fn(&str) -> Option<String>) -> Result<PathBuf> {
        if let Some(p) = env("PFAR_CONFIG_PATH") {
            return Ok(PathBuf::from(p));
        }
        Ok(PathBuf::from("config.toml"))
    }

    /// Apply environment variable overrides (env > config > defaults).
    ///
    /// Takes a resolver function for testability (avoids unsafe `set_var` in tests).
    fn apply_overrides(&mut self, env: impl Fn(&str) -> Option<String>) {
        // Kernel.
        if let Some(v) = env("PFAR_SHUTDOWN_TIMEOUT_SECS") {
            match v.parse() {
                Ok(n) => self.kernel.shutdown_timeout_seconds = n,
                Err(_) => tracing::warn!(
                    var = "PFAR_SHUTDOWN_TIMEOUT_SECS",
                    value = %v,
                    "ignoring invalid env override"
                ),
            }
        }

        // Paths.
        if let Some(v) = env("PFAR_AUDIT_LOG") {
            self.paths.audit_log = v;
        }
        if let Some(v) = env("PFAR_JOURNAL_PATH") {
            self.paths.journal_db = v;
        }

        // LLM — local.
        if let Some(v) = env("PFAR_OLLAMA_URL") {
            self.llm.local.base_url = v;
        }
        if let Some(v) = env("PFAR_LOCAL_MODEL") {
            self.llm.local.model = v;
        }

        // LLM — Anthropic (env var presence creates the provider).
        if let Some(key) = env("PFAR_ANTHROPIC_API_KEY") {
            let model = env("PFAR_ANTHROPIC_MODEL").unwrap_or_else(|| {
                self.llm
                    .anthropic
                    .as_ref()
                    .map(|c| c.model.clone())
                    .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string())
            });
            self.llm.anthropic = Some(LlmCloudConfig {
                api_key: key,
                model,
            });
        }

        // LLM — OpenAI.
        if let Some(key) = env("PFAR_OPENAI_API_KEY") {
            let model = env("PFAR_OPENAI_MODEL").unwrap_or_else(|| {
                self.llm
                    .openai
                    .as_ref()
                    .map(|c| c.model.clone())
                    .unwrap_or_else(|| "gpt-4o".to_string())
            });
            let base_url = self
                .llm
                .openai
                .as_ref()
                .map(|c| c.base_url.clone())
                .unwrap_or_else(|| "https://api.openai.com".to_string());
            self.llm.openai = Some(LlmOpenAiConfig {
                base_url,
                api_key: key,
                model,
            });
        }

        // LLM — LM Studio.
        if let Some(url) = env("PFAR_LMSTUDIO_URL") {
            let model = env("PFAR_LMSTUDIO_MODEL").unwrap_or_else(|| {
                self.llm
                    .lmstudio
                    .as_ref()
                    .map(|c| c.model.clone())
                    .unwrap_or_else(|| "deepseek-r1".to_string())
            });
            self.llm.lmstudio = Some(LlmLocalServerConfig {
                base_url: url,
                model,
            });
        }

        // Telegram adapter.
        if let Some(v) = env("PFAR_TELEGRAM_BOT_TOKEN") {
            self.adapter.telegram.bot_token = Some(v);
        }
        if let Some(v) = env("PFAR_TELEGRAM_OWNER_ID") {
            self.adapter.telegram.owner_id = v;
        }
    }

    /// Parse a TOML string into config (for testing).
    pub fn from_toml(toml_str: &str) -> Result<Self> {
        let config: PfarConfig = toml::from_str(toml_str).context("failed to parse config TOML")?;
        Ok(config)
    }
}

// ── Kernel config ───────────────────────────────────────────────

/// Kernel core settings (spec 18.1 `[kernel]`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct KernelConfig {
    /// Tracing log level filter (spec 14.5).
    pub log_level: String,
    /// Sink for admin notifications (spec 6.6).
    pub admin_sink: String,
    /// Approval queue timeout in seconds (spec 6.6).
    pub approval_timeout_seconds: u64,
    /// Channel buffer size for adapter <-> kernel mpsc.
    pub channel_buffer_size: usize,
    /// Graceful shutdown timeout in seconds (spec 14.1).
    pub shutdown_timeout_seconds: u64,
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            log_level: "info".to_string(),
            admin_sink: "sink:telegram:owner".to_string(),
            approval_timeout_seconds: 300,
            channel_buffer_size: 100,
            shutdown_timeout_seconds: 30,
        }
    }
}

// ── Paths config ────────────────────────────────────────────────

/// Filesystem paths for persistent state (spec 18.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PathsConfig {
    /// Audit log JSONL path (spec 6.7).
    pub audit_log: String,
    /// Journal SQLite database path (persistence spec).
    pub journal_db: String,
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            audit_log: "/tmp/pfar-audit.jsonl".to_string(),
            journal_db: "/tmp/pfar-journal.db".to_string(),
        }
    }
}

// ── LLM config ──────────────────────────────────────────────────

/// LLM provider configuration (spec 18.1 `[llm]`, spec 11.2).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    /// Local Ollama provider — always available (spec 11.2).
    pub local: LlmLocalConfig,
    /// Anthropic provider (spec 11.2).
    pub anthropic: Option<LlmCloudConfig>,
    /// OpenAI provider (spec 11.2).
    pub openai: Option<LlmOpenAiConfig>,
    /// LM Studio provider (local OpenAI-compatible).
    pub lmstudio: Option<LlmLocalServerConfig>,
}

/// Local Ollama provider config (spec 11.2).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LlmLocalConfig {
    /// Ollama base URL.
    pub base_url: String,
    /// Model name.
    #[serde(alias = "default_model")]
    pub model: String,
}

impl Default for LlmLocalConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434".to_string(),
            model: "llama3".to_string(),
        }
    }
}

/// Cloud LLM provider config for Anthropic (spec 11.2).
#[derive(Clone, Deserialize)]
pub struct LlmCloudConfig {
    /// API key (or vault reference like `"vault:anthropic_api_key"`).
    pub api_key: String,
    /// Model name.
    #[serde(default = "default_anthropic_model", alias = "default_model")]
    pub model: String,
}

impl std::fmt::Debug for LlmCloudConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmCloudConfig")
            .field("api_key", &"__REDACTED__")
            .field("model", &self.model)
            .finish()
    }
}

fn default_anthropic_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}

/// OpenAI provider config (spec 11.2).
#[derive(Clone, Deserialize)]
pub struct LlmOpenAiConfig {
    /// API base URL.
    #[serde(default = "default_openai_base_url")]
    pub base_url: String,
    /// API key (or vault reference).
    pub api_key: String,
    /// Model name.
    #[serde(default = "default_openai_model", alias = "default_model")]
    pub model: String,
}

impl std::fmt::Debug for LlmOpenAiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmOpenAiConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"__REDACTED__")
            .field("model", &self.model)
            .finish()
    }
}

fn default_openai_base_url() -> String {
    "https://api.openai.com".to_string()
}

fn default_openai_model() -> String {
    "gpt-4o".to_string()
}

/// LM Studio provider config (local OpenAI-compatible server).
#[derive(Debug, Clone, Deserialize)]
pub struct LlmLocalServerConfig {
    /// Server base URL.
    pub base_url: String,
    /// Model name.
    #[serde(default = "default_lmstudio_model", alias = "default_model")]
    pub model: String,
}

fn default_lmstudio_model() -> String {
    "deepseek-r1".to_string()
}

// ── Adapter config ──────────────────────────────────────────────

/// Adapter configuration (spec 18.1 `[adapter]`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AdapterConfig {
    /// Telegram adapter settings (spec 18.1).
    pub telegram: TelegramAdapterConfig,
}

/// Telegram adapter configuration (spec 18.1).
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct TelegramAdapterConfig {
    /// Whether the adapter is enabled.
    pub enabled: bool,
    /// Bot token (or vault reference).
    pub bot_token: Option<String>,
    /// Owner's Telegram user ID.
    pub owner_id: String,
    /// Long-poll timeout in seconds.
    pub poll_timeout_seconds: u32,
}

impl std::fmt::Debug for TelegramAdapterConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramAdapterConfig")
            .field("enabled", &self.enabled)
            .field(
                "bot_token",
                &self.bot_token.as_ref().map(|_| "__REDACTED__"),
            )
            .field("owner_id", &self.owner_id)
            .field("poll_timeout_seconds", &self.poll_timeout_seconds)
            .finish()
    }
}

impl Default for TelegramAdapterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bot_token: None,
            owner_id: "415494855".to_string(),
            poll_timeout_seconds: 30,
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_matches_current_constants() {
        let config = PfarConfig::default();

        // Kernel defaults.
        assert_eq!(config.kernel.log_level, "info");
        assert_eq!(config.kernel.admin_sink, "sink:telegram:owner");
        assert_eq!(config.kernel.approval_timeout_seconds, 300);
        assert_eq!(config.kernel.channel_buffer_size, 100);
        assert_eq!(config.kernel.shutdown_timeout_seconds, 30);

        // Paths defaults.
        assert_eq!(config.paths.audit_log, "/tmp/pfar-audit.jsonl");
        assert_eq!(config.paths.journal_db, "/tmp/pfar-journal.db");

        // LLM defaults.
        assert_eq!(config.llm.local.base_url, "http://localhost:11434");
        assert_eq!(config.llm.local.model, "llama3");
        assert!(config.llm.anthropic.is_none());
        assert!(config.llm.openai.is_none());
        assert!(config.llm.lmstudio.is_none());

        // Adapter defaults.
        assert!(config.adapter.telegram.enabled);
        assert!(config.adapter.telegram.bot_token.is_none());
        assert_eq!(config.adapter.telegram.owner_id, "415494855");
        assert_eq!(config.adapter.telegram.poll_timeout_seconds, 30);
    }

    #[test]
    fn test_parse_full_toml() {
        let toml_str = r#"
[kernel]
log_level = "debug"
admin_sink = "sink:slack:owner"
approval_timeout_seconds = 600
channel_buffer_size = 200
shutdown_timeout_seconds = 60

[paths]
audit_log = "/home/igor/.pfar/audit.jsonl"
journal_db = "/home/igor/.pfar/journal.db"

[llm.local]
base_url = "http://localhost:11435"
model = "qwen3-8b"

[llm.anthropic]
api_key = "vault:anthropic_api_key"
model = "claude-sonnet-4-20250514"

[llm.openai]
base_url = "https://api.openai.com"
api_key = "vault:openai_api_key"
model = "gpt-4o-mini"

[llm.lmstudio]
base_url = "http://localhost:1234"
model = "deepseek-r1-8b"

[adapter.telegram]
enabled = true
bot_token = "vault:telegram_bot_token"
owner_id = "123456789"
poll_timeout_seconds = 45
"#;

        let config = PfarConfig::from_toml(toml_str).expect("should parse");

        assert_eq!(config.kernel.log_level, "debug");
        assert_eq!(config.kernel.approval_timeout_seconds, 600);
        assert_eq!(config.kernel.channel_buffer_size, 200);
        assert_eq!(config.kernel.shutdown_timeout_seconds, 60);
        assert_eq!(config.paths.audit_log, "/home/igor/.pfar/audit.jsonl");
        assert_eq!(config.paths.journal_db, "/home/igor/.pfar/journal.db");
        assert_eq!(config.llm.local.base_url, "http://localhost:11435");
        assert_eq!(config.llm.local.model, "qwen3-8b");

        let anthropic = config
            .llm
            .anthropic
            .as_ref()
            .expect("anthropic should exist");
        assert_eq!(anthropic.api_key, "vault:anthropic_api_key");

        let openai = config.llm.openai.as_ref().expect("openai should exist");
        assert_eq!(openai.model, "gpt-4o-mini");

        let lmstudio = config.llm.lmstudio.as_ref().expect("lmstudio should exist");
        assert_eq!(lmstudio.base_url, "http://localhost:1234");

        assert_eq!(config.adapter.telegram.owner_id, "123456789");
        assert_eq!(config.adapter.telegram.poll_timeout_seconds, 45);
    }

    #[test]
    fn test_parse_partial_toml_uses_defaults() {
        let toml_str = r#"
[kernel]
log_level = "warn"
"#;

        let config = PfarConfig::from_toml(toml_str).expect("should parse");

        // Overridden value.
        assert_eq!(config.kernel.log_level, "warn");

        // Everything else is default.
        assert_eq!(config.kernel.approval_timeout_seconds, 300);
        assert_eq!(config.paths.audit_log, "/tmp/pfar-audit.jsonl");
        assert_eq!(config.llm.local.base_url, "http://localhost:11434");
        assert!(config.adapter.telegram.bot_token.is_none());
    }

    #[test]
    fn test_parse_empty_toml_uses_defaults() {
        let config = PfarConfig::from_toml("").expect("should parse empty");
        let default = PfarConfig::default();

        assert_eq!(config.kernel.log_level, default.kernel.log_level);
        assert_eq!(config.paths.audit_log, default.paths.audit_log);
        assert_eq!(config.llm.local.base_url, default.llm.local.base_url);
        assert_eq!(
            config.adapter.telegram.owner_id,
            default.adapter.telegram.owner_id
        );
    }

    #[test]
    fn test_env_overrides_config_values() {
        let toml_str = r#"
[paths]
audit_log = "/from/toml/audit.jsonl"
journal_db = "/from/toml/journal.db"

[kernel]
shutdown_timeout_seconds = 60
"#;

        let mut config = PfarConfig::from_toml(toml_str).expect("should parse");

        // Simulate env vars.
        let env = |key: &str| -> Option<String> {
            match key {
                "PFAR_AUDIT_LOG" => Some("/from/env/audit.jsonl".to_string()),
                "PFAR_SHUTDOWN_TIMEOUT_SECS" => Some("15".to_string()),
                _ => None,
            }
        };
        config.apply_overrides(env);

        // Env wins over file.
        assert_eq!(config.paths.audit_log, "/from/env/audit.jsonl");
        assert_eq!(config.kernel.shutdown_timeout_seconds, 15);

        // File value kept when no env override.
        assert_eq!(config.paths.journal_db, "/from/toml/journal.db");
    }

    #[test]
    fn test_env_creates_anthropic_provider() {
        let mut config = PfarConfig::default();
        assert!(config.llm.anthropic.is_none());

        let env = |key: &str| -> Option<String> {
            match key {
                "PFAR_ANTHROPIC_API_KEY" => Some("sk-test-123".to_string()),
                "PFAR_ANTHROPIC_MODEL" => Some("claude-opus-4-20250514".to_string()),
                _ => None,
            }
        };
        config.apply_overrides(env);

        let anthropic = config.llm.anthropic.as_ref().expect("should be created");
        assert_eq!(anthropic.api_key, "sk-test-123");
        assert_eq!(anthropic.model, "claude-opus-4-20250514");
    }

    #[test]
    fn test_env_creates_openai_provider() {
        let mut config = PfarConfig::default();
        assert!(config.llm.openai.is_none());

        let env = |key: &str| -> Option<String> {
            match key {
                "PFAR_OPENAI_API_KEY" => Some("sk-openai-test".to_string()),
                _ => None,
            }
        };
        config.apply_overrides(env);

        let openai = config.llm.openai.as_ref().expect("should be created");
        assert_eq!(openai.api_key, "sk-openai-test");
        assert_eq!(openai.model, "gpt-4o"); // default model
        assert_eq!(openai.base_url, "https://api.openai.com"); // default url
    }

    #[test]
    fn test_env_creates_lmstudio_provider() {
        let mut config = PfarConfig::default();
        assert!(config.llm.lmstudio.is_none());

        let env = |key: &str| -> Option<String> {
            match key {
                "PFAR_LMSTUDIO_URL" => Some("http://localhost:1234".to_string()),
                _ => None,
            }
        };
        config.apply_overrides(env);

        let lmstudio = config.llm.lmstudio.as_ref().expect("should be created");
        assert_eq!(lmstudio.base_url, "http://localhost:1234");
        assert_eq!(lmstudio.model, "deepseek-r1"); // default model
    }

    #[test]
    fn test_config_path_uses_env_var() {
        let path = PfarConfig::config_path_with(|key| match key {
            "PFAR_CONFIG_PATH" => Some("/custom/config.toml".to_string()),
            _ => None,
        })
        .expect("should resolve");

        assert_eq!(path, PathBuf::from("/custom/config.toml"));
    }

    #[test]
    fn test_config_path_defaults_to_cwd() {
        let path = PfarConfig::config_path_with(|_| None).expect("should resolve");
        assert_eq!(path, PathBuf::from("config.toml"));
    }

    #[test]
    fn test_invalid_toml_returns_error() {
        let result = PfarConfig::from_toml("this is {{ not valid toml");
        assert!(result.is_err());
    }

    #[test]
    fn test_telegram_env_overrides() {
        let mut config = PfarConfig::default();

        let env = |key: &str| -> Option<String> {
            match key {
                "PFAR_TELEGRAM_BOT_TOKEN" => Some("bot:token:123".to_string()),
                "PFAR_TELEGRAM_OWNER_ID" => Some("999888777".to_string()),
                _ => None,
            }
        };
        config.apply_overrides(env);

        assert_eq!(
            config.adapter.telegram.bot_token.as_deref(),
            Some("bot:token:123")
        );
        assert_eq!(config.adapter.telegram.owner_id, "999888777");
    }

    #[test]
    fn test_ollama_url_env_override() {
        let mut config = PfarConfig::default();

        let env = |key: &str| -> Option<String> {
            match key {
                "PFAR_OLLAMA_URL" => Some("http://gpu-server:11434".to_string()),
                _ => None,
            }
        };
        config.apply_overrides(env);

        assert_eq!(config.llm.local.base_url, "http://gpu-server:11434");
    }
}
