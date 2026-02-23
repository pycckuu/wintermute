//! Tests for flatline configuration loading and defaults.

use std::io::Write;

use flatline::config::{flatline_paths, load_flatline_config, FlatlineConfig};

#[test]
fn parse_complete_config() {
    let toml_content = r#"
[model]
default = "ollama/llama3:8b"
fallback = "anthropic/claude-haiku-4-5-20251001"

[budget]
max_tokens_per_day = 200_000

[checks]
interval_secs = 120
health_stale_threshold_secs = 60

[thresholds]
tool_failure_rate = 0.3
tool_failure_window_hours = 2
budget_burn_rate_alert = 0.9
memory_pending_alert = 50
unused_tool_days = 14
max_tool_count_warning = 20
disk_warning_gb = 10.0

[auto_fix]
enabled = false
restart_on_crash = false
quarantine_failing_tools = false
disable_failing_tasks = false
revert_recent_changes = false
max_auto_restarts_per_hour = 5

[reports]
daily_health = "09:30"
alert_cooldown_mins = 15
telegram_prefix = "TestPrefix"

[telegram]
bot_token_env = "MY_BOT_TOKEN"
notify_users = [111, 222]
"#;

    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("flatline.toml");
    let mut f = std::fs::File::create(&config_path).expect("create file");
    f.write_all(toml_content.as_bytes()).expect("write");

    let config = load_flatline_config(&config_path).expect("parse config");

    assert_eq!(config.model.default, "ollama/llama3:8b");
    assert_eq!(
        config.model.fallback.as_deref(),
        Some("anthropic/claude-haiku-4-5-20251001")
    );
    assert_eq!(config.budget.max_tokens_per_day, 200_000);
    assert_eq!(config.checks.interval_secs, 120);
    assert_eq!(config.checks.health_stale_threshold_secs, 60);
    assert!((config.thresholds.tool_failure_rate - 0.3).abs() < f64::EPSILON);
    assert_eq!(config.thresholds.tool_failure_window_hours, 2);
    assert!((config.thresholds.budget_burn_rate_alert - 0.9).abs() < f64::EPSILON);
    assert_eq!(config.thresholds.memory_pending_alert, 50);
    assert_eq!(config.thresholds.unused_tool_days, 14);
    assert_eq!(config.thresholds.max_tool_count_warning, 20);
    assert!((config.thresholds.disk_warning_gb - 10.0).abs() < f64::EPSILON);
    assert!(!config.auto_fix.enabled);
    assert!(!config.auto_fix.restart_on_crash);
    assert!(!config.auto_fix.quarantine_failing_tools);
    assert!(!config.auto_fix.disable_failing_tasks);
    assert!(!config.auto_fix.revert_recent_changes);
    assert_eq!(config.auto_fix.max_auto_restarts_per_hour, 5);
    assert_eq!(config.reports.daily_health, "09:30");
    assert_eq!(config.reports.alert_cooldown_mins, 15);
    assert_eq!(config.reports.telegram_prefix, "TestPrefix");
    assert_eq!(config.telegram.bot_token_env, "MY_BOT_TOKEN");
    assert_eq!(config.telegram.notify_users, vec![111, 222]);
}

#[test]
fn parse_minimal_config_uses_defaults() {
    let toml_content = "";

    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("flatline.toml");
    std::fs::write(&config_path, toml_content).expect("write");

    let config = load_flatline_config(&config_path).expect("parse empty config");

    assert_eq!(config.model.default, "ollama/qwen3:8b");
    assert!(config.model.fallback.is_none());
    assert_eq!(config.budget.max_tokens_per_day, 100_000);
    assert_eq!(config.checks.interval_secs, 300);
    assert_eq!(config.checks.health_stale_threshold_secs, 180);
    assert!((config.thresholds.tool_failure_rate - 0.5).abs() < f64::EPSILON);
    assert_eq!(config.thresholds.tool_failure_window_hours, 1);
    assert!(config.auto_fix.enabled);
    assert!(config.auto_fix.restart_on_crash);
    assert_eq!(config.auto_fix.max_auto_restarts_per_hour, 3);
    assert_eq!(config.reports.daily_health, "08:00");
    assert_eq!(config.reports.alert_cooldown_mins, 30);
    assert_eq!(config.telegram.bot_token_env, "WINTERMUTE_TELEGRAM_TOKEN");
    assert!(config.telegram.notify_users.is_empty());
}

#[test]
fn parse_with_missing_sections_uses_defaults() {
    let toml_content = r#"
[model]
default = "ollama/custom:7b"
"#;

    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("flatline.toml");
    std::fs::write(&config_path, toml_content).expect("write");

    let config = load_flatline_config(&config_path).expect("parse partial config");

    // Specified value.
    assert_eq!(config.model.default, "ollama/custom:7b");

    // All other sections should have defaults.
    assert_eq!(config.budget.max_tokens_per_day, 100_000);
    assert_eq!(config.checks.interval_secs, 300);
    assert!(config.auto_fix.enabled);
    assert_eq!(config.reports.daily_health, "08:00");
    assert!(config.telegram.notify_users.is_empty());
}

#[test]
fn flatline_paths_resolves_correctly() {
    let paths = flatline_paths().expect("resolve paths");

    // Should be under ~/.wintermute/flatline/
    assert!(paths.root.ends_with("flatline"));
    assert!(paths.state_db.ends_with("state.db"));
    assert!(paths.diagnoses_dir.ends_with("diagnoses"));
    assert!(paths.patches_dir.ends_with("patches"));

    // state_db should be under root.
    assert!(paths.state_db.starts_with(&paths.root));
    assert!(paths.diagnoses_dir.starts_with(&paths.root));
    assert!(paths.patches_dir.starts_with(&paths.root));
}

#[test]
fn parse_example_config_file() {
    let example = include_str!("../../flatline.toml.example");
    let config: FlatlineConfig = toml::from_str(example).expect("parse example config");
    assert_eq!(config.model.default, "ollama/qwen3:8b");
    assert_eq!(config.checks.interval_secs, 300);
}
