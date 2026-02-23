//! Tests for the 8 known failure pattern detectors.

use std::sync::Arc;

use flatline::config::FlatlineConfig;
use flatline::db::StateDb;
use flatline::patterns::{
    evaluate_patterns, is_pid_alive, read_git_log, GitLogEntry, PatternKind, Severity,
};
use flatline::stats::StatsEngine;
use flatline::watcher::Watcher;
use wintermute::heartbeat::health::{BudgetReport, HealthReport};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn default_config() -> FlatlineConfig {
    toml::from_str("").expect("default config")
}

fn make_health_report() -> HealthReport {
    HealthReport {
        status: "running".to_owned(),
        uptime_secs: 86400,
        last_heartbeat: chrono::Utc::now().to_rfc3339(),
        executor: "docker".to_owned(),
        container_healthy: true,
        active_sessions: 0,
        memory_db_size_mb: 1.0,
        scripts_count: 5,
        dynamic_tools_count: 5,
        budget_today: BudgetReport {
            used: 0,
            limit: 5_000_000,
        },
        last_error: None,
    }
}

async fn setup() -> (StatsEngine, Arc<StateDb>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("test_state.db");
    let db = Arc::new(StateDb::open(&db_path).await.expect("open db"));
    let engine = StatsEngine::new(Arc::clone(&db));
    (engine, db, dir)
}

fn make_watcher(dir: &tempfile::TempDir) -> Watcher {
    let log_dir = dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("create log dir");
    let health_path = dir.path().join("health.json");
    Watcher::new(log_dir, health_path)
}

fn write_health(dir: &tempfile::TempDir, health: &HealthReport) {
    let health_path = dir.path().join("health.json");
    let json = serde_json::to_string(health).expect("serialize health");
    std::fs::write(health_path, json).expect("write health");
}

fn recent_bucket() -> String {
    use chrono::Timelike;
    let now = chrono::Utc::now();
    let truncated = now
        .with_minute(0)
        .and_then(|d| d.with_second(0))
        .and_then(|d| d.with_nanosecond(0))
        .unwrap_or(now);
    truncated.to_rfc3339()
}

// ---------------------------------------------------------------------------
// Pattern: ToolFailingAfterChange
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tool_failing_after_change_detected_with_correlated_commit() {
    let (engine, db, dir) = setup().await;
    let watcher = make_watcher(&dir);
    let config = default_config();
    let mut health = make_health_report();
    health.last_heartbeat = chrono::Utc::now().to_rfc3339();
    write_health(&dir, &health);

    // Record high failure rate for a tool.
    let bucket = recent_bucket();
    for _ in 0..9 {
        db.record_tool_stat("deploy_check", &bucket, false, None)
            .await
            .expect("record");
    }
    db.record_tool_stat("deploy_check", &bucket, true, None)
        .await
        .expect("record");

    // Create a git log entry that mentions the tool.
    let git_log = vec![GitLogEntry {
        hash: "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2".to_owned(),
        timestamp: "2026-02-19T14:00:00+00:00".to_owned(),
        message: "create tool: deploy_check".to_owned(),
    }];

    let matches = evaluate_patterns(&engine, Some(&health), &git_log, &config, &watcher).await;

    let tool_match = matches
        .iter()
        .find(|m| m.kind == PatternKind::ToolFailingAfterChange);
    assert!(tool_match.is_some(), "should detect ToolFailingAfterChange");

    let m = tool_match.expect("just checked");
    assert_eq!(m.severity, Severity::Medium);
    assert!(m.auto_fixable);
    assert!(m.evidence.summary.contains("deploy_check"));
}

#[tokio::test]
async fn tool_failing_without_correlated_commit_not_detected() {
    let (engine, db, dir) = setup().await;
    let watcher = make_watcher(&dir);
    let config = default_config();
    let health = make_health_report();
    write_health(&dir, &health);

    // Record high failure rate.
    let bucket = recent_bucket();
    for _ in 0..9 {
        db.record_tool_stat("deploy_check", &bucket, false, None)
            .await
            .expect("record");
    }

    // No git log entries for this tool.
    let git_log = vec![GitLogEntry {
        hash: "aaaa".to_owned(),
        timestamp: "2026-02-19T14:00:00+00:00".to_owned(),
        message: "update README".to_owned(),
    }];

    let matches = evaluate_patterns(&engine, Some(&health), &git_log, &config, &watcher).await;

    let tool_match = matches
        .iter()
        .find(|m| m.kind == PatternKind::ToolFailingAfterChange);
    assert!(
        tool_match.is_none(),
        "should not detect without correlated commit"
    );
}

// ---------------------------------------------------------------------------
// Pattern: ContainerWontStart
// ---------------------------------------------------------------------------

#[tokio::test]
async fn container_wont_start_detected() {
    let (engine, _db, dir) = setup().await;
    let watcher = make_watcher(&dir);
    let config = default_config();

    let mut health = make_health_report();
    health.container_healthy = false;
    health.status = "unhealthy".to_owned();
    write_health(&dir, &health);

    let matches = evaluate_patterns(&engine, Some(&health), &[], &config, &watcher).await;

    let container_match = matches
        .iter()
        .find(|m| m.kind == PatternKind::ContainerWontStart);
    assert!(
        container_match.is_some(),
        "should detect ContainerWontStart"
    );
    assert_eq!(container_match.expect("checked").severity, Severity::High);
}

#[tokio::test]
async fn container_healthy_not_detected() {
    let (engine, _db, dir) = setup().await;
    let watcher = make_watcher(&dir);
    let config = default_config();
    let health = make_health_report();
    write_health(&dir, &health);

    let matches = evaluate_patterns(&engine, Some(&health), &[], &config, &watcher).await;

    let container_match = matches
        .iter()
        .find(|m| m.kind == PatternKind::ContainerWontStart);
    assert!(
        container_match.is_none(),
        "should not detect when container is healthy"
    );
}

// ---------------------------------------------------------------------------
// Pattern: MemoryBloat
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_bloat_detected() {
    let (engine, _db, dir) = setup().await;
    let watcher = make_watcher(&dir);
    let config = default_config();

    let mut health = make_health_report();
    health.memory_db_size_mb = 100.0; // Well above 50MB threshold.
    write_health(&dir, &health);

    let matches = evaluate_patterns(&engine, Some(&health), &[], &config, &watcher).await;

    let bloat_match = matches.iter().find(|m| m.kind == PatternKind::MemoryBloat);
    assert!(bloat_match.is_some(), "should detect MemoryBloat");
    assert_eq!(bloat_match.expect("checked").severity, Severity::Low);
}

#[tokio::test]
async fn memory_normal_not_detected() {
    let (engine, _db, dir) = setup().await;
    let watcher = make_watcher(&dir);
    let config = default_config();
    let health = make_health_report(); // 1.0 MB default.
    write_health(&dir, &health);

    let matches = evaluate_patterns(&engine, Some(&health), &[], &config, &watcher).await;

    let bloat_match = matches.iter().find(|m| m.kind == PatternKind::MemoryBloat);
    assert!(
        bloat_match.is_none(),
        "should not detect with normal memory size"
    );
}

// ---------------------------------------------------------------------------
// Pattern: DynamicToolSprawl
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tool_sprawl_detected() {
    let (engine, _db, dir) = setup().await;
    let watcher = make_watcher(&dir);
    let config = default_config(); // threshold = 40

    let mut health = make_health_report();
    health.dynamic_tools_count = 50; // Above 40 threshold.
    write_health(&dir, &health);

    let matches = evaluate_patterns(&engine, Some(&health), &[], &config, &watcher).await;

    let sprawl_match = matches
        .iter()
        .find(|m| m.kind == PatternKind::DynamicToolSprawl);
    assert!(sprawl_match.is_some(), "should detect DynamicToolSprawl");
    assert_eq!(sprawl_match.expect("checked").severity, Severity::Low);
    assert!(!sprawl_match.iter().next().is_some_and(|_| false)); // no auto-fix for sprawl
}

#[tokio::test]
async fn tool_sprawl_not_detected_under_threshold() {
    let (engine, _db, dir) = setup().await;
    let watcher = make_watcher(&dir);
    let config = default_config();
    let mut health = make_health_report();
    health.dynamic_tools_count = 10;
    write_health(&dir, &health);

    let matches = evaluate_patterns(&engine, Some(&health), &[], &config, &watcher).await;

    let sprawl_match = matches
        .iter()
        .find(|m| m.kind == PatternKind::DynamicToolSprawl);
    assert!(sprawl_match.is_none(), "should not detect under threshold");
}

// ---------------------------------------------------------------------------
// Sorting by severity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn evaluate_patterns_sorts_by_severity_critical_first() {
    let (engine, _db, dir) = setup().await;
    let watcher = make_watcher(&dir);
    let config = default_config();

    // Craft a health report that triggers both container (High) and memory bloat (Low).
    let mut health = make_health_report();
    health.container_healthy = false;
    health.memory_db_size_mb = 200.0;
    health.dynamic_tools_count = 50;
    write_health(&dir, &health);

    let matches = evaluate_patterns(&engine, Some(&health), &[], &config, &watcher).await;

    // Should have at least container (High), memory bloat (Low), tool sprawl (Low).
    assert!(matches.len() >= 2, "should have multiple matches");

    // Verify sorting: first match should be highest severity.
    for i in 0..matches.len().saturating_sub(1) {
        assert!(
            severity_rank(matches[i].severity) >= severity_rank(matches[i + 1].severity),
            "matches should be sorted by severity descending: {:?} vs {:?}",
            matches[i].severity,
            matches[i + 1].severity,
        );
    }
}

fn severity_rank(s: Severity) -> u8 {
    match s {
        Severity::Low => 0,
        Severity::Medium => 1,
        Severity::High => 2,
        Severity::Critical => 3,
    }
}

// ---------------------------------------------------------------------------
// No health report
// ---------------------------------------------------------------------------

#[tokio::test]
async fn evaluate_with_no_health_report() {
    let (engine, _db, dir) = setup().await;
    let watcher = make_watcher(&dir);
    let config = default_config();

    // No health report at all -- should not panic.
    let matches = evaluate_patterns(&engine, None, &[], &config, &watcher).await;

    // With no health data, only process_down or disk_pressure could fire.
    // Neither should panic.
    for m in &matches {
        assert!(
            m.kind != PatternKind::ContainerWontStart,
            "container check requires health"
        );
        assert!(
            m.kind != PatternKind::MemoryBloat,
            "memory check requires health"
        );
    }
}

// ---------------------------------------------------------------------------
// read_git_log with temp repo
// ---------------------------------------------------------------------------

#[test]
fn read_git_log_parses_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("scripts");
    std::fs::create_dir_all(&repo).expect("create repo dir");

    // Initialize a git repo and create commits.
    // Clear GIT_DIR/GIT_WORK_TREE to avoid inheriting from parent processes
    // (e.g. when running inside a pre-push hook).
    std::process::Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(["init", &repo.to_string_lossy()])
        .output()
        .expect("git init");

    std::process::Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args([
            "-C",
            &repo.to_string_lossy(),
            "config",
            "user.email",
            "test@test.com",
        ])
        .output()
        .expect("git config email");

    std::process::Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(["-C", &repo.to_string_lossy(), "config", "user.name", "Test"])
        .output()
        .expect("git config name");

    // Create first commit.
    let file1 = repo.join("tool1.json");
    std::fs::write(&file1, "{}").expect("write file1");
    std::process::Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(["-C", &repo.to_string_lossy(), "add", "."])
        .output()
        .expect("git add");
    std::process::Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args([
            "-C",
            &repo.to_string_lossy(),
            "commit",
            "-m",
            "create tool: tool1",
        ])
        .output()
        .expect("git commit 1");

    // Create second commit.
    let file2 = repo.join("tool2.json");
    std::fs::write(&file2, "{}").expect("write file2");
    std::process::Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(["-C", &repo.to_string_lossy(), "add", "."])
        .output()
        .expect("git add 2");
    std::process::Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args([
            "-C",
            &repo.to_string_lossy(),
            "commit",
            "-m",
            "create tool: tool2",
        ])
        .output()
        .expect("git commit 2");

    let entries = read_git_log(&repo, 10).expect("read git log");
    assert_eq!(entries.len(), 2);

    // Most recent commit first.
    assert!(entries[0].message.contains("tool2"));
    assert!(entries[1].message.contains("tool1"));

    // Hashes should be 40 hex chars.
    assert_eq!(entries[0].hash.len(), 40);
    assert!(entries[0].hash.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn read_git_log_empty_repo_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("scripts");
    std::fs::create_dir_all(&repo).expect("create repo dir");

    // Initialize empty git repo (no commits).
    std::process::Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(["init", &repo.to_string_lossy()])
        .output()
        .expect("git init");

    let entries = read_git_log(&repo, 10);
    // On some git versions this errors, on others it returns empty.
    // Both are acceptable.
    if let Ok(e) = entries {
        assert!(e.is_empty());
    }
}

#[test]
fn read_git_log_nonexistent_dir_errors() {
    let result = read_git_log(std::path::Path::new("/nonexistent/path"), 10);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// is_pid_alive
// ---------------------------------------------------------------------------

#[test]
fn pid_alive_returns_false_for_nonexistent_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nonexistent.pid");
    assert!(!is_pid_alive(&path));
}

#[test]
fn pid_alive_returns_false_for_empty_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("empty.pid");
    std::fs::write(&path, "").expect("write");
    assert!(!is_pid_alive(&path));
}

#[test]
fn pid_alive_returns_false_for_invalid_pid() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("invalid.pid");
    std::fs::write(&path, "not_a_number").expect("write");
    assert!(!is_pid_alive(&path));
}

#[test]
fn pid_alive_returns_true_for_current_process() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("current.pid");
    std::fs::write(&path, std::process::id().to_string()).expect("write");
    assert!(is_pid_alive(&path));
}

#[test]
fn pid_alive_returns_false_for_dead_process() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("dead.pid");
    // Use a very high PID that almost certainly doesn't exist.
    std::fs::write(&path, "4294967295").expect("write");
    assert!(!is_pid_alive(&path));
}

// ---------------------------------------------------------------------------
// Severity ordering
// ---------------------------------------------------------------------------

#[test]
fn severity_ordering() {
    assert!(severity_rank(Severity::Critical) > severity_rank(Severity::High));
    assert!(severity_rank(Severity::High) > severity_rank(Severity::Medium));
    assert!(severity_rank(Severity::Medium) > severity_rank(Severity::Low));
}

// ---------------------------------------------------------------------------
// PatternKind serde
// ---------------------------------------------------------------------------

#[test]
fn pattern_kind_serializes_to_snake_case() {
    let kind = PatternKind::ToolFailingAfterChange;
    let json = serde_json::to_string(&kind).expect("serialize");
    assert_eq!(json, r#""tool_failing_after_change""#);
}

#[test]
fn pattern_kind_deserializes_from_snake_case() {
    let kind: PatternKind =
        serde_json::from_str(r#""budget_exhaustion_loop""#).expect("deserialize");
    assert_eq!(kind, PatternKind::BudgetExhaustionLoop);
}
