//! Tests for `src/heartbeat/health.rs` — health report serialization and file writing.

use wintermute::heartbeat::health::{BudgetReport, HealthReport};

#[test]
fn health_report_serializes_to_json() {
    let report = HealthReport {
        status: "running".to_owned(),
        uptime_secs: 3600,
        last_heartbeat: "2025-01-01T00:00:00Z".to_owned(),
        executor: "Docker".to_owned(),
        container_healthy: true,
        active_sessions: 2,
        memory_db_size_mb: 1.5,
        scripts_count: 10,
        dynamic_tools_count: 10,
        budget_today: BudgetReport {
            used: 50_000,
            limit: 5_000_000,
        },
        last_error: None,
    };

    let json = serde_json::to_string_pretty(&report).expect("should serialize");

    assert!(json.contains("\"status\": \"running\""));
    assert!(json.contains("\"uptime_secs\": 3600"));
    assert!(json.contains("\"container_healthy\": true"));
    assert!(json.contains("\"active_sessions\": 2"));
    assert!(json.contains("\"last_error\": null"));
}

#[test]
fn health_report_with_error() {
    let report = HealthReport {
        status: "degraded".to_owned(),
        uptime_secs: 60,
        last_heartbeat: "2025-01-01T00:01:00Z".to_owned(),
        executor: "Direct".to_owned(),
        container_healthy: false,
        active_sessions: 0,
        memory_db_size_mb: 0.1,
        scripts_count: 0,
        dynamic_tools_count: 0,
        budget_today: BudgetReport {
            used: 0,
            limit: 5_000_000,
        },
        last_error: Some("container not found".to_owned()),
    };

    let json = serde_json::to_string(&report).expect("should serialize");
    assert!(json.contains("container not found"));
    assert!(json.contains("\"status\":\"degraded\""));
}

#[tokio::test]
async fn write_health_file_creates_file() {
    let tmp = tempfile::tempdir().expect("should create temp dir");
    let path = tmp.path().join("health.json");

    let report = HealthReport {
        status: "running".to_owned(),
        uptime_secs: 100,
        last_heartbeat: "2025-01-01T00:00:00Z".to_owned(),
        executor: "Docker".to_owned(),
        container_healthy: true,
        active_sessions: 1,
        memory_db_size_mb: 0.5,
        scripts_count: 5,
        dynamic_tools_count: 5,
        budget_today: BudgetReport {
            used: 1000,
            limit: 5_000_000,
        },
        last_error: None,
    };

    wintermute::heartbeat::health::write_health_file(&report, &path)
        .await
        .expect("write should succeed");

    assert!(path.exists(), "health.json should exist");

    let content = std::fs::read_to_string(&path).expect("should read file");
    let parsed: serde_json::Value = serde_json::from_str(&content).expect("should parse as JSON");
    assert_eq!(parsed["status"], "running");
    assert_eq!(parsed["uptime_secs"], 100);
}

#[tokio::test]
async fn write_health_file_is_atomic() {
    let tmp = tempfile::tempdir().expect("should create temp dir");
    let path = tmp.path().join("health.json");

    // Write twice — second write should overwrite cleanly.
    for i in 0_u64..2 {
        let report = HealthReport {
            status: format!("run-{i}"),
            uptime_secs: i,
            last_heartbeat: format!("2025-01-01T00:0{i}:00Z"),
            executor: "Docker".to_owned(),
            container_healthy: true,
            active_sessions: 0,
            memory_db_size_mb: 0.0,
            scripts_count: 0,
            dynamic_tools_count: 0,
            budget_today: BudgetReport {
                used: 0,
                limit: 5_000_000,
            },
            last_error: None,
        };

        wintermute::heartbeat::health::write_health_file(&report, &path)
            .await
            .expect("write should succeed");
    }

    let content = std::fs::read_to_string(&path).expect("should read file");
    let parsed: serde_json::Value = serde_json::from_str(&content).expect("should parse");
    assert_eq!(parsed["status"], "run-1", "last write should win");

    // Temp file should not linger.
    let tmp_path = path.with_extension("json.tmp");
    assert!(!tmp_path.exists(), "temp file should be cleaned up");
}
