//! Coverage for config parsing and path resolution.

use std::collections::HashMap;
use std::path::Path;

use wintermute::config::{
    all_model_specs, config_dir, runtime_paths, AgentConfig, BudgetConfig, Config, EgressConfig,
    HeartbeatConfig, LearningConfig, ModelsConfig, PersonalityConfig, PrivacyConfig, PromotionMode,
    SandboxConfig,
};

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

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
    assert!(sandbox.runtime.is_none());
}

#[test]
fn sandbox_config_is_clone() {
    let sandbox = SandboxConfig::default();
    let cloned = sandbox.clone();
    assert_eq!(cloned.memory_mb, sandbox.memory_mb);
}

#[test]
fn default_personality_values() {
    let personality = PersonalityConfig::default();
    assert_eq!(personality.name, "Wintermute");
    assert!(personality.soul.is_empty());
}

#[test]
fn default_heartbeat_values() {
    let heartbeat = HeartbeatConfig::default();
    assert!(heartbeat.enabled);
    assert_eq!(heartbeat.interval_secs, 60);
}

#[test]
fn default_learning_values() {
    let learning = LearningConfig::default();
    assert!(learning.enabled);
    assert_eq!(learning.promotion_mode, PromotionMode::Auto);
    assert_eq!(learning.auto_promote_threshold, 3);
}

#[test]
fn default_egress_values() {
    let egress = EgressConfig::default();
    assert!(egress.allowed_domains.is_empty());
    assert_eq!(egress.fetch_rate_limit, 30);
    assert_eq!(egress.request_rate_limit, 10);
}

#[test]
fn default_privacy_values() {
    let privacy = PrivacyConfig::default();
    assert!(privacy.always_approve_domains.is_empty());
    assert!(privacy.blocked_domains.is_empty());
}

#[test]
fn default_promotion_mode_is_auto() {
    assert_eq!(PromotionMode::default(), PromotionMode::Auto);
}

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

#[test]
fn config_dir_resolves() {
    let dir = config_dir();
    assert!(dir.is_ok());
    let path = dir.expect("config dir should resolve");
    assert!(path.ends_with(".wintermute"));
}

#[test]
fn runtime_paths_use_data_directory_for_memory_db() {
    let paths = runtime_paths().expect("runtime paths should resolve");
    let expected_suffix = Path::new(".wintermute").join("data").join("memory.db");
    assert!(paths.memory_db.ends_with(expected_suffix));
}

#[test]
fn runtime_paths_has_expected_children() {
    let paths = runtime_paths().expect("runtime paths should resolve");
    assert!(paths.config_toml.ends_with("config.toml"));
    assert!(paths.agent_toml.ends_with("agent.toml"));
    assert!(paths.env_file.ends_with(".env"));
    assert!(paths.scripts_dir.ends_with("scripts"));
    assert!(paths.workspace_dir.ends_with("workspace"));
    assert!(paths.backups_dir.ends_with("backups"));
}

// ---------------------------------------------------------------------------
// TOML parsing
// ---------------------------------------------------------------------------

#[test]
fn parse_minimal_config() {
    let toml_str = r#"
[models]
default = "anthropic/claude-sonnet-4-5-20250929"

[channels.telegram]
bot_token_env = "WINTERMUTE_TELEGRAM_TOKEN"
allowed_users = [123456789]
"#;
    let config: Config = toml::from_str(toml_str).expect("minimal config should parse");
    assert_eq!(
        config.models.default,
        "anthropic/claude-sonnet-4-5-20250929"
    );
    assert_eq!(config.channels.telegram.allowed_users, vec![123456789]);
}

#[test]
fn parse_config_with_sandbox_runtime() {
    let toml_str = r#"
[models]
default = "ollama/qwen3:8b"

[channels.telegram]
bot_token_env = "TOK"
allowed_users = [1]

[sandbox]
memory_mb = 4096
cpu_cores = 4.0
runtime = "runsc"
"#;
    let config: Config = toml::from_str(toml_str).expect("config with runtime should parse");
    assert_eq!(config.sandbox.memory_mb, 4096);
    assert_eq!(config.sandbox.runtime, Some("runsc".to_owned()));
}

#[test]
fn parse_agent_config_with_defaults() {
    let toml_str = r#"
[personality]
name = "Wintermute"

[heartbeat]
enabled = true
interval_secs = 60
"#;
    let agent: AgentConfig = toml::from_str(toml_str).expect("agent config should parse");
    assert_eq!(agent.personality.name, "Wintermute");
    assert!(agent.heartbeat.enabled);
    assert_eq!(agent.heartbeat.interval_secs, 60);
    assert!(agent.learning.enabled);
}

#[test]
fn parse_promotion_mode_variants() {
    // Parse via a wrapper struct since TOML requires key-value pairs.
    #[derive(serde::Deserialize)]
    struct Wrapper {
        mode: PromotionMode,
    }

    let auto: Wrapper = toml::from_str("mode = \"auto\"").expect("auto should parse");
    assert_eq!(auto.mode, PromotionMode::Auto);

    let suggest: Wrapper = toml::from_str("mode = \"suggest\"").expect("suggest should parse");
    assert_eq!(suggest.mode, PromotionMode::Suggest);

    let off: Wrapper = toml::from_str("mode = \"off\"").expect("off should parse");
    assert_eq!(off.mode, PromotionMode::Off);
}

#[test]
fn parse_privacy_config() {
    let toml_str = r#"
[models]
default = "anthropic/claude-sonnet-4-5-20250929"

[channels.telegram]
bot_token_env = "WINTERMUTE_TELEGRAM_TOKEN"
allowed_users = [123456789]

[privacy]
always_approve_domains = ["example.com"]
blocked_domains = ["blocked.example"]
"#;
    let config: Config = toml::from_str(toml_str).expect("privacy config should parse");
    assert_eq!(config.privacy.always_approve_domains, vec!["example.com"]);
    assert_eq!(config.privacy.blocked_domains, vec!["blocked.example"]);
}

#[test]
fn parse_scheduled_task_with_enabled_field() {
    let toml_str = r#"
[[scheduled_tasks]]
name = "daily_backup"
cron = "0 3 * * *"
builtin = "backup"
enabled = false
"#;
    let agent: AgentConfig = toml::from_str(toml_str).expect("scheduled task should parse");
    assert_eq!(agent.scheduled_tasks.len(), 1);
    assert!(!agent.scheduled_tasks[0].enabled);
}

#[test]
fn scheduled_task_enabled_defaults_to_true() {
    let toml_str = r#"
[[scheduled_tasks]]
name = "daily_backup"
cron = "0 3 * * *"
builtin = "backup"
"#;
    let agent: AgentConfig = toml::from_str(toml_str).expect("scheduled task should parse");
    assert!(agent.scheduled_tasks[0].enabled);
}

// ---------------------------------------------------------------------------
// all_model_specs
// ---------------------------------------------------------------------------

#[test]
fn all_model_specs_deduplicates() {
    let models = ModelsConfig {
        default: "ollama/qwen3:8b".to_owned(),
        roles: HashMap::from([("observer".to_owned(), "ollama/qwen3:8b".to_owned())]),
        skills: HashMap::new(),
    };
    let specs = all_model_specs(&models);
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0], "ollama/qwen3:8b");
}

#[test]
fn all_model_specs_preserves_order() {
    let models = ModelsConfig {
        default: "ollama/qwen3:8b".to_owned(),
        roles: HashMap::from([("observer".to_owned(), "anthropic/claude-haiku".to_owned())]),
        skills: HashMap::from([("deploy".to_owned(), "anthropic/claude-sonnet".to_owned())]),
    };
    let specs = all_model_specs(&models);
    assert_eq!(specs[0], "ollama/qwen3:8b");
    assert!(specs.contains(&"anthropic/claude-haiku".to_owned()));
    assert!(specs.contains(&"anthropic/claude-sonnet".to_owned()));
}
