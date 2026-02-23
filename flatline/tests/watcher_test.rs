//! Tests for the log watcher and health file monitor.

use std::io::Write;

use flatline::watcher::Watcher;

#[test]
fn poll_logs_parses_jsonl() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_dir = dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("create log dir");

    let log_file = log_dir.join("wintermute.log.2026-02-19.jsonl");
    let mut f = std::fs::File::create(&log_file).expect("create log file");

    writeln!(
        f,
        r#"{{"ts":"2026-02-19T14:30:00Z","level":"info","event":"tool_call","tool":"news_digest","duration_ms":1200,"success":true}}"#
    )
    .expect("write line 1");
    writeln!(
        f,
        r#"{{"ts":"2026-02-19T14:30:05Z","level":"error","event":"tool_call","tool":"deploy_check","duration_ms":30000,"success":false,"error":"timeout"}}"#
    )
    .expect("write line 2");

    let health_path = dir.path().join("health.json");
    let mut watcher = Watcher::new(log_dir, health_path);

    let events = watcher.poll_logs().expect("poll logs");
    assert_eq!(events.len(), 2);

    assert_eq!(events[0].tool.as_deref(), Some("news_digest"));
    assert_eq!(events[0].success, Some(true));
    assert_eq!(events[0].duration_ms, Some(1200));

    assert_eq!(events[1].tool.as_deref(), Some("deploy_check"));
    assert_eq!(events[1].success, Some(false));
    assert_eq!(events[1].error.as_deref(), Some("timeout"));
}

#[test]
fn poll_logs_skips_unparsable_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_dir = dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("create log dir");

    let log_file = log_dir.join("test.jsonl");
    let mut f = std::fs::File::create(&log_file).expect("create log file");

    writeln!(f, "this is not json").expect("write bad line");
    writeln!(f, r#"{{"ts":"2026-01-01T00:00:00Z","level":"info"}}"#).expect("write good line");
    writeln!(f, "another bad line").expect("write bad line 2");

    let health_path = dir.path().join("health.json");
    let mut watcher = Watcher::new(log_dir, health_path);

    let events = watcher.poll_logs().expect("poll logs");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].ts.as_deref(), Some("2026-01-01T00:00:00Z"));
}

#[test]
fn poll_logs_incremental_reads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_dir = dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("create log dir");

    let log_file = log_dir.join("test.jsonl");
    let health_path = dir.path().join("health.json");
    let mut watcher = Watcher::new(log_dir, health_path);

    // Write first line.
    {
        let mut f = std::fs::File::create(&log_file).expect("create");
        writeln!(
            f,
            r#"{{"ts":"2026-01-01T00:00:00Z","level":"info","event":"first"}}"#
        )
        .expect("write");
    }

    let events1 = watcher.poll_logs().expect("poll 1");
    assert_eq!(events1.len(), 1);
    assert_eq!(events1[0].event.as_deref(), Some("first"));

    // Append second line.
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&log_file)
            .expect("open append");
        writeln!(
            f,
            r#"{{"ts":"2026-01-01T01:00:00Z","level":"info","event":"second"}}"#
        )
        .expect("write");
    }

    let events2 = watcher.poll_logs().expect("poll 2");
    assert_eq!(events2.len(), 1);
    assert_eq!(events2[0].event.as_deref(), Some("second"));
}

#[test]
fn poll_logs_empty_dir_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_dir = dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("create log dir");

    let health_path = dir.path().join("health.json");
    let mut watcher = Watcher::new(log_dir, health_path);

    let events = watcher.poll_logs().expect("poll empty");
    assert!(events.is_empty());
}

#[test]
fn poll_logs_nonexistent_dir_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_dir = dir.path().join("does_not_exist");
    let health_path = dir.path().join("health.json");
    let mut watcher = Watcher::new(log_dir, health_path);

    let events = watcher.poll_logs().expect("poll nonexistent");
    assert!(events.is_empty());
}

#[test]
fn read_health_parses_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let health_path = dir.path().join("health.json");

    let health_json = r#"{
        "status": "running",
        "uptime_secs": 86400,
        "last_heartbeat": "2026-02-19T14:30:00Z",
        "executor": "docker",
        "container_healthy": true,
        "active_sessions": 1,
        "memory_db_size_mb": 12.0,
        "scripts_count": 23,
        "dynamic_tools_count": 23,
        "budget_today": { "used": 120000, "limit": 5000000 },
        "last_error": null
    }"#;

    std::fs::write(&health_path, health_json).expect("write health.json");

    let log_dir = dir.path().join("logs");
    let watcher = Watcher::new(log_dir, health_path);

    let report = watcher.read_health().expect("read health");
    assert_eq!(report.status, "running");
    assert_eq!(report.uptime_secs, 86400);
    assert!(report.container_healthy);
    assert_eq!(report.budget_today.used, 120000);
    assert_eq!(report.budget_today.limit, 5000000);
}

#[test]
fn is_health_stale_with_old_timestamp() {
    let dir = tempfile::tempdir().expect("tempdir");
    let health_path = dir.path().join("health.json");

    // Set last_heartbeat to 10 minutes ago.
    let ten_min_ago = chrono::Utc::now() - chrono::Duration::seconds(600);
    let health_json = format!(
        r#"{{
        "status": "running",
        "uptime_secs": 86400,
        "last_heartbeat": "{}",
        "executor": "docker",
        "container_healthy": true,
        "active_sessions": 0,
        "memory_db_size_mb": 0.0,
        "scripts_count": 0,
        "dynamic_tools_count": 0,
        "budget_today": {{ "used": 0, "limit": 100000 }},
        "last_error": null
    }}"#,
        ten_min_ago.to_rfc3339()
    );

    std::fs::write(&health_path, health_json).expect("write");

    let log_dir = dir.path().join("logs");
    let watcher = Watcher::new(log_dir, health_path);

    // 180 seconds threshold should mark 600 seconds ago as stale.
    assert!(watcher.is_health_stale(180).expect("stale check"));

    // 900 seconds threshold should not mark 600 seconds ago as stale.
    assert!(!watcher.is_health_stale(900).expect("not stale check"));
}

#[test]
fn is_health_stale_with_fresh_timestamp() {
    let dir = tempfile::tempdir().expect("tempdir");
    let health_path = dir.path().join("health.json");

    let now = chrono::Utc::now();
    let health_json = format!(
        r#"{{
        "status": "running",
        "uptime_secs": 100,
        "last_heartbeat": "{}",
        "executor": "docker",
        "container_healthy": true,
        "active_sessions": 0,
        "memory_db_size_mb": 0.0,
        "scripts_count": 0,
        "dynamic_tools_count": 0,
        "budget_today": {{ "used": 0, "limit": 100000 }},
        "last_error": null
    }}"#,
        now.to_rfc3339()
    );

    std::fs::write(&health_path, health_json).expect("write");

    let log_dir = dir.path().join("logs");
    let watcher = Watcher::new(log_dir, health_path);

    assert!(!watcher.is_health_stale(180).expect("fresh check"));
}
