//! Tests for the fix lifecycle: propose, apply, and verify.

use flatline::config::FlatlineConfig;
use flatline::fixer::{apply_fix, propose_fix, validate_commit_hash, FixAction, FixStatus};
use flatline::patterns::{Evidence, PatternKind, PatternMatch, Severity};
use wintermute::config::RuntimePaths;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn default_config() -> FlatlineConfig {
    toml::from_str("").expect("default config")
}

fn make_pattern_match(kind: PatternKind, severity: Severity, auto_fixable: bool) -> PatternMatch {
    PatternMatch {
        kind,
        severity,
        evidence: Evidence {
            summary: format!("test pattern: {kind:?}"),
            details: serde_json::json!({}),
        },
        auto_fixable,
    }
}

fn make_pattern_with_details(
    kind: PatternKind,
    severity: Severity,
    auto_fixable: bool,
    details: serde_json::Value,
) -> PatternMatch {
    PatternMatch {
        kind,
        severity,
        evidence: Evidence {
            summary: format!("test pattern: {kind:?}"),
            details,
        },
        auto_fixable,
    }
}

fn temp_runtime_paths(dir: &tempfile::TempDir) -> RuntimePaths {
    let root = dir.path().to_path_buf();
    let scripts_dir = root.join("scripts");
    std::fs::create_dir_all(&scripts_dir).expect("create scripts dir");

    RuntimePaths {
        root: root.clone(),
        config_toml: root.join("config.toml"),
        agent_toml: root.join("agent.toml"),
        env_file: root.join(".env"),
        scripts_dir,
        workspace_dir: root.join("workspace"),
        data_dir: root.join("data"),
        backups_dir: root.join("backups"),
        memory_db: root.join("data/memory.db"),
        pid_file: root.join("wintermute.pid"),
        health_json: root.join("health.json"),
        identity_md: root.join("IDENTITY.md"),
        user_md: root.join("USER.md"),
    }
}

// ---------------------------------------------------------------------------
// validate_commit_hash
// ---------------------------------------------------------------------------

#[test]
fn validate_commit_hash_accepts_valid_hex() {
    assert!(validate_commit_hash("a1b2c3d4").is_ok());
    assert!(validate_commit_hash("AABBCCDD").is_ok());
    assert!(validate_commit_hash("0123456789abcdef0123456789abcdef01234567").is_ok());
}

#[test]
fn validate_commit_hash_rejects_empty() {
    assert!(validate_commit_hash("").is_err());
}

#[test]
fn validate_commit_hash_rejects_non_hex() {
    assert!(validate_commit_hash("not-hex").is_err());
    assert!(validate_commit_hash("abc123xyz").is_err());
}

#[test]
fn validate_commit_hash_rejects_shell_injection() {
    assert!(validate_commit_hash("abc; rm -rf /").is_err());
    assert!(validate_commit_hash("abc && echo pwned").is_err());
    assert!(validate_commit_hash("$(whoami)").is_err());
    assert!(validate_commit_hash("`whoami`").is_err());
    assert!(validate_commit_hash("abc|cat /etc/passwd").is_err());
}

#[test]
fn validate_commit_hash_rejects_whitespace() {
    assert!(validate_commit_hash("abc 123").is_err());
    assert!(validate_commit_hash("abc\t123").is_err());
    assert!(validate_commit_hash("abc\n123").is_err());
}

// ---------------------------------------------------------------------------
// propose_fix — mapping from PatternKind to FixAction
// ---------------------------------------------------------------------------

#[test]
fn propose_fix_process_down_restart() {
    let config = default_config();
    let pattern = make_pattern_match(PatternKind::ProcessDown, Severity::Critical, true);
    let fix = propose_fix(&pattern, &config);

    assert!(fix.pattern.as_deref().is_some());
    let action: FixAction =
        serde_json::from_str(fix.action.as_deref().expect("action")).expect("parse action");
    assert_eq!(action, FixAction::RestartProcess);
    assert!(fix.diagnosis.is_some());
}

#[test]
fn propose_fix_process_down_report_only_when_disabled() {
    let config: FlatlineConfig = toml::from_str(
        r#"
        [auto_fix]
        restart_on_crash = false
        "#,
    )
    .expect("parse config");

    let pattern = make_pattern_match(PatternKind::ProcessDown, Severity::Critical, true);
    let fix = propose_fix(&pattern, &config);

    let action: FixAction =
        serde_json::from_str(fix.action.as_deref().expect("action")).expect("parse action");
    assert!(matches!(action, FixAction::ReportOnly { .. }));
}

#[test]
fn propose_fix_container_wont_start() {
    let config = default_config();
    let pattern = make_pattern_match(PatternKind::ContainerWontStart, Severity::High, true);
    let fix = propose_fix(&pattern, &config);

    let action: FixAction =
        serde_json::from_str(fix.action.as_deref().expect("action")).expect("parse action");
    assert_eq!(action, FixAction::ResetSandbox);
}

#[test]
fn propose_fix_budget_exhaustion_report_only() {
    let config = default_config();
    let pattern = make_pattern_match(PatternKind::BudgetExhaustionLoop, Severity::Medium, false);
    let fix = propose_fix(&pattern, &config);

    let action: FixAction =
        serde_json::from_str(fix.action.as_deref().expect("action")).expect("parse action");
    assert!(matches!(action, FixAction::ReportOnly { .. }));
}

#[test]
fn propose_fix_tool_failing_quarantine() {
    let config = default_config();
    let pattern = make_pattern_with_details(
        PatternKind::ToolFailingAfterChange,
        Severity::Medium,
        true,
        serde_json::json!({
            "tool": "deploy_check",
            "commit_hash": "a1b2c3d4",
        }),
    );
    let fix = propose_fix(&pattern, &config);

    let action: FixAction =
        serde_json::from_str(fix.action.as_deref().expect("action")).expect("parse action");
    assert_eq!(
        action,
        FixAction::QuarantineTool {
            tool_name: "deploy_check".to_owned()
        }
    );
}

#[test]
fn propose_fix_scheduled_task_disable() {
    let config = default_config();
    let pattern = make_pattern_with_details(
        PatternKind::ScheduledTaskFailing,
        Severity::Medium,
        true,
        serde_json::json!({
            "task_name": "news_digest",
        }),
    );
    let fix = propose_fix(&pattern, &config);

    let action: FixAction =
        serde_json::from_str(fix.action.as_deref().expect("action")).expect("parse action");
    assert_eq!(
        action,
        FixAction::DisableScheduledTask {
            task_name: "news_digest".to_owned()
        }
    );
}

#[test]
fn propose_fix_memory_bloat_report_only() {
    let config = default_config();
    let pattern = make_pattern_match(PatternKind::MemoryBloat, Severity::Low, false);
    let fix = propose_fix(&pattern, &config);

    let action: FixAction =
        serde_json::from_str(fix.action.as_deref().expect("action")).expect("parse action");
    assert!(matches!(action, FixAction::ReportOnly { .. }));
}

#[test]
fn propose_fix_tool_sprawl_report_only() {
    let config = default_config();
    let pattern = make_pattern_match(PatternKind::DynamicToolSprawl, Severity::Low, false);
    let fix = propose_fix(&pattern, &config);

    let action: FixAction =
        serde_json::from_str(fix.action.as_deref().expect("action")).expect("parse action");
    assert!(matches!(action, FixAction::ReportOnly { .. }));
}

#[test]
fn propose_fix_disk_pressure_prune() {
    let config = default_config();
    let pattern = make_pattern_match(PatternKind::DiskSpacePressure, Severity::Medium, true);
    let fix = propose_fix(&pattern, &config);

    let action: FixAction =
        serde_json::from_str(fix.action.as_deref().expect("action")).expect("parse action");
    assert!(matches!(action, FixAction::PruneLogs { retention_days: 7 }));
}

#[test]
fn propose_fix_generates_unique_ids() {
    let config = default_config();
    let pattern = make_pattern_match(PatternKind::ProcessDown, Severity::Critical, true);
    let fix1 = propose_fix(&pattern, &config);
    let fix2 = propose_fix(&pattern, &config);

    assert_ne!(fix1.id, fix2.id, "fix IDs should be unique");
}

// ---------------------------------------------------------------------------
// apply_fix: QuarantineTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn apply_quarantine_tool_renames_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = temp_runtime_paths(&dir);

    // Create a tool JSON file.
    let tool_file = paths.scripts_dir.join("bad_tool.json");
    std::fs::write(&tool_file, r#"{"name": "bad_tool"}"#).expect("write tool file");

    let fix = flatline::db::FixRecord {
        id: "fix-test".to_owned(),
        detected_at: chrono::Utc::now().to_rfc3339(),
        pattern: Some("ToolFailingAfterChange".to_owned()),
        diagnosis: Some("test".to_owned()),
        action: Some(
            serde_json::to_string(&FixAction::QuarantineTool {
                tool_name: "bad_tool".to_owned(),
            })
            .expect("serialize"),
        ),
        applied_at: None,
        verified: None,
        user_notified: false,
    };

    apply_fix(&fix, &paths).await.expect("apply fix");

    // Original file should be gone.
    assert!(!tool_file.exists(), "original file should be removed");

    // Quarantined file should exist.
    let quarantined = paths.scripts_dir.join("bad_tool.json.quarantined");
    assert!(quarantined.exists(), "quarantined file should exist");

    // Content should be preserved.
    let contents = std::fs::read_to_string(quarantined).expect("read quarantined");
    assert_eq!(contents, r#"{"name": "bad_tool"}"#);
}

#[tokio::test]
async fn apply_quarantine_tool_skips_missing_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = temp_runtime_paths(&dir);

    let fix = flatline::db::FixRecord {
        id: "fix-test".to_owned(),
        detected_at: chrono::Utc::now().to_rfc3339(),
        pattern: None,
        diagnosis: None,
        action: Some(
            serde_json::to_string(&FixAction::QuarantineTool {
                tool_name: "nonexistent".to_owned(),
            })
            .expect("serialize"),
        ),
        applied_at: None,
        verified: None,
        user_notified: false,
    };

    // Should not error on missing file.
    apply_fix(&fix, &paths)
        .await
        .expect("apply fix should succeed");
}

#[tokio::test]
async fn apply_quarantine_tool_rejects_path_traversal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = temp_runtime_paths(&dir);

    let fix = flatline::db::FixRecord {
        id: "fix-test".to_owned(),
        detected_at: chrono::Utc::now().to_rfc3339(),
        pattern: None,
        diagnosis: None,
        action: Some(
            serde_json::to_string(&FixAction::QuarantineTool {
                tool_name: "../etc/passwd".to_owned(),
            })
            .expect("serialize"),
        ),
        applied_at: None,
        verified: None,
        user_notified: false,
    };

    let result = apply_fix(&fix, &paths).await;
    assert!(result.is_err(), "should reject path traversal");
}

// ---------------------------------------------------------------------------
// apply_fix: PruneLogs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn apply_prune_logs_deletes_old_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = temp_runtime_paths(&dir);

    let logs_dir = paths.root.join("logs");
    std::fs::create_dir_all(&logs_dir).expect("create logs dir");

    // Create a "recent" log file.
    let recent = logs_dir.join("recent.jsonl");
    std::fs::write(&recent, "recent log line").expect("write recent");

    // Create an "old" log file and backdate its mtime.
    let old = logs_dir.join("old.jsonl");
    std::fs::write(&old, "old log line").expect("write old");

    // Set mtime to 30 days ago.
    let thirty_days_ago = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(30 * 86400))
        .expect("time math");
    let mtime = filetime::FileTime::from_system_time(thirty_days_ago);
    filetime::set_file_mtime(&old, mtime).expect("set mtime");

    let fix = flatline::db::FixRecord {
        id: "fix-prune".to_owned(),
        detected_at: chrono::Utc::now().to_rfc3339(),
        pattern: None,
        diagnosis: None,
        action: Some(
            serde_json::to_string(&FixAction::PruneLogs { retention_days: 7 }).expect("serialize"),
        ),
        applied_at: None,
        verified: None,
        user_notified: false,
    };

    apply_fix(&fix, &paths).await.expect("apply prune");

    // Recent file should still exist.
    assert!(recent.exists(), "recent file should remain");

    // Old file should be deleted.
    assert!(!old.exists(), "old file should be pruned");
}

#[tokio::test]
async fn apply_prune_logs_no_logs_dir_ok() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = temp_runtime_paths(&dir);
    // Don't create logs dir.

    let fix = flatline::db::FixRecord {
        id: "fix-prune".to_owned(),
        detected_at: chrono::Utc::now().to_rfc3339(),
        pattern: None,
        diagnosis: None,
        action: Some(
            serde_json::to_string(&FixAction::PruneLogs { retention_days: 7 }).expect("serialize"),
        ),
        applied_at: None,
        verified: None,
        user_notified: false,
    };

    // Should succeed even without a logs directory.
    apply_fix(&fix, &paths).await.expect("apply prune no-op");
}

// ---------------------------------------------------------------------------
// apply_fix: DisableScheduledTask
// ---------------------------------------------------------------------------

#[tokio::test]
async fn apply_disable_scheduled_task_modifies_toml() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = temp_runtime_paths(&dir);

    // Write an agent.toml with a scheduled task.
    let agent_toml = r#"
[[scheduled_tasks]]
name = "news_digest"
cron = "0 8 * * *"
tool = "news_digest"
enabled = true

[[scheduled_tasks]]
name = "backup"
cron = "0 3 * * *"
builtin = "backup"
enabled = true
"#;
    std::fs::write(&paths.agent_toml, agent_toml).expect("write agent.toml");

    let fix = flatline::db::FixRecord {
        id: "fix-disable".to_owned(),
        detected_at: chrono::Utc::now().to_rfc3339(),
        pattern: None,
        diagnosis: None,
        action: Some(
            serde_json::to_string(&FixAction::DisableScheduledTask {
                task_name: "news_digest".to_owned(),
            })
            .expect("serialize"),
        ),
        applied_at: None,
        verified: None,
        user_notified: false,
    };

    apply_fix(&fix, &paths).await.expect("apply disable task");

    // Re-read and verify.
    let updated = std::fs::read_to_string(&paths.agent_toml).expect("read updated");
    let doc: toml::Value = toml::from_str(&updated).expect("parse updated");

    let tasks = doc
        .get("scheduled_tasks")
        .and_then(|v| v.as_array())
        .expect("tasks array");

    for task in tasks {
        let name = task.get("name").and_then(|v| v.as_str()).expect("name");
        let enabled = task
            .get("enabled")
            .and_then(|v| v.as_bool())
            .expect("enabled");

        if name == "news_digest" {
            assert!(!enabled, "news_digest should be disabled");
        } else if name == "backup" {
            assert!(enabled, "backup should remain enabled");
        }
    }
}

#[tokio::test]
async fn apply_disable_scheduled_task_unknown_task_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = temp_runtime_paths(&dir);

    let agent_toml = r#"
[[scheduled_tasks]]
name = "backup"
cron = "0 3 * * *"
builtin = "backup"
enabled = true
"#;
    std::fs::write(&paths.agent_toml, agent_toml).expect("write agent.toml");

    let fix = flatline::db::FixRecord {
        id: "fix-disable".to_owned(),
        detected_at: chrono::Utc::now().to_rfc3339(),
        pattern: None,
        diagnosis: None,
        action: Some(
            serde_json::to_string(&FixAction::DisableScheduledTask {
                task_name: "nonexistent".to_owned(),
            })
            .expect("serialize"),
        ),
        applied_at: None,
        verified: None,
        user_notified: false,
    };

    let result = apply_fix(&fix, &paths).await;
    assert!(result.is_err(), "should error for unknown task");
}

// ---------------------------------------------------------------------------
// apply_fix: ReportOnly (no-op)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn apply_report_only_succeeds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = temp_runtime_paths(&dir);

    let fix = flatline::db::FixRecord {
        id: "fix-report".to_owned(),
        detected_at: chrono::Utc::now().to_rfc3339(),
        pattern: None,
        diagnosis: None,
        action: Some(
            serde_json::to_string(&FixAction::ReportOnly {
                message: "just a report".to_owned(),
            })
            .expect("serialize"),
        ),
        applied_at: None,
        verified: None,
        user_notified: false,
    };

    apply_fix(&fix, &paths)
        .await
        .expect("report only should succeed");
}

// ---------------------------------------------------------------------------
// apply_fix: RestartProcess (cold start — no PID file)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn apply_restart_process_cold_start_no_pid_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = temp_runtime_paths(&dir);
    // Do NOT create a PID file — simulates cold start.

    let fix = flatline::db::FixRecord {
        id: "fix-cold-start".to_owned(),
        detected_at: chrono::Utc::now().to_rfc3339(),
        pattern: Some("ProcessDown".to_owned()),
        diagnosis: Some("cold start".to_owned()),
        action: Some(serde_json::to_string(&FixAction::RestartProcess).expect("serialize")),
        applied_at: None,
        verified: None,
        user_notified: false,
    };

    // Should NOT error with "failed to read PID file".
    // The spawn itself may fail (no `wintermute` binary), but the PID
    // read step must be gracefully skipped.
    let result = apply_fix(&fix, &paths).await;
    match result {
        Ok(()) => {} // wintermute binary happened to be on PATH
        Err(e) => {
            let msg = format!("{e:?}");
            assert!(
                !msg.contains("failed to read PID file"),
                "should not fail on missing PID file; got: {msg}"
            );
        }
    }
}

#[tokio::test]
async fn apply_restart_process_empty_pid_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = temp_runtime_paths(&dir);
    // Create an empty PID file.
    std::fs::write(&paths.pid_file, "").expect("write empty pid");

    let fix = flatline::db::FixRecord {
        id: "fix-empty-pid".to_owned(),
        detected_at: chrono::Utc::now().to_rfc3339(),
        pattern: Some("ProcessDown".to_owned()),
        diagnosis: Some("empty pid file".to_owned()),
        action: Some(serde_json::to_string(&FixAction::RestartProcess).expect("serialize")),
        applied_at: None,
        verified: None,
        user_notified: false,
    };

    // Empty PID should be handled gracefully (skip SIGTERM, proceed to spawn).
    let result = apply_fix(&fix, &paths).await;
    match result {
        Ok(()) => {}
        Err(e) => {
            let msg = format!("{e:?}");
            assert!(
                !msg.contains("failed to read PID file"),
                "should handle empty PID file gracefully; got: {msg}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// start_wintermute public wrapper
// ---------------------------------------------------------------------------

#[tokio::test]
async fn start_wintermute_delegates_to_restart_process() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = temp_runtime_paths(&dir);
    // No PID file — cold start path.

    let result = flatline::fixer::start_wintermute(&paths).await;
    match result {
        Ok(()) => {}
        Err(e) => {
            let msg = format!("{e:?}");
            // Should not fail on PID file; only a spawn error is acceptable.
            assert!(
                !msg.contains("failed to read PID file"),
                "start_wintermute should handle missing PID; got: {msg}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// FixAction serde roundtrip
// ---------------------------------------------------------------------------

#[test]
fn fix_action_serde_roundtrip() {
    let actions = vec![
        FixAction::RestartProcess,
        FixAction::ResetSandbox,
        FixAction::GitRevert {
            commit_hash: "abc123".to_owned(),
        },
        FixAction::QuarantineTool {
            tool_name: "bad_tool".to_owned(),
        },
        FixAction::DisableScheduledTask {
            task_name: "news_digest".to_owned(),
        },
        FixAction::PruneLogs { retention_days: 7 },
        FixAction::ReportOnly {
            message: "hello".to_owned(),
        },
    ];

    for action in &actions {
        let json = serde_json::to_string(action).expect("serialize");
        let deserialized: FixAction = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(action, &deserialized, "roundtrip failed for {action:?}");
    }
}

// ---------------------------------------------------------------------------
// FixStatus serde
// ---------------------------------------------------------------------------

#[test]
fn fix_status_serde() {
    let statuses = vec![
        FixStatus::Detected,
        FixStatus::Diagnosed,
        FixStatus::Proposed,
        FixStatus::Approved,
        FixStatus::Applied,
        FixStatus::Verified,
        FixStatus::Failed,
    ];

    for status in &statuses {
        let json = serde_json::to_string(status).expect("serialize");
        let deserialized: FixStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(status, &deserialized, "roundtrip failed for {status:?}");
    }
}
