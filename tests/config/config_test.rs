//! Coverage for config parsing and path resolution.

use std::path::Path;

use wintermute::config::{
    config_dir, runtime_paths, AgentConfig, BudgetConfig, Config, SandboxConfig,
};

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
    let path = match dir {
        Ok(path) => path,
        Err(err) => panic!("config dir should resolve: {err}"),
    };
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
    let config_parse = toml::from_str::<Config>(toml_str);
    assert!(config_parse.is_ok());
    let config = match config_parse {
        Ok(config) => config,
        Err(err) => panic!("minimal config should parse: {err}"),
    };
    assert_eq!(
        config.models.default,
        "anthropic/claude-sonnet-4-5-20250929"
    );
    assert_eq!(config.channels.telegram.allowed_users, vec![123456789]);
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
    let parsed = toml::from_str::<AgentConfig>(toml_str);
    assert!(parsed.is_ok());
    let agent = match parsed {
        Ok(agent) => agent,
        Err(err) => panic!("agent config should parse: {err}"),
    };

    assert_eq!(agent.personality.name, "Wintermute");
    assert!(agent.heartbeat.enabled);
    assert_eq!(agent.heartbeat.interval_secs, 60);
    assert!(agent.learning.enabled);
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
    let parsed = toml::from_str::<Config>(toml_str);
    assert!(parsed.is_ok());
    let config = match parsed {
        Ok(config) => config,
        Err(err) => panic!("privacy config should parse: {err}"),
    };
    assert_eq!(config.privacy.always_approve_domains, vec!["example.com"]);
    assert_eq!(config.privacy.blocked_domains, vec!["blocked.example"]);
}

#[test]
fn runtime_paths_use_data_directory_for_memory_db() {
    let paths_result = runtime_paths();
    assert!(paths_result.is_ok());
    let paths = match paths_result {
        Ok(paths) => paths,
        Err(err) => panic!("runtime paths should resolve: {err}"),
    };
    let expected_suffix = Path::new(".wintermute").join("data").join("memory.db");
    assert!(paths.memory_db.ends_with(expected_suffix));
}
