//! Tests for the rolling statistics engine.

use std::sync::Arc;

use flatline::db::StateDb;
use flatline::stats::StatsEngine;
use flatline::watcher::LogEvent;
use wintermute::heartbeat::health::{BudgetReport, HealthReport};

/// Generate an hourly bucket timestamp for "now" (truncated to current hour).
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

async fn setup() -> (StatsEngine, Arc<StateDb>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("test_state.db");
    let db = Arc::new(StateDb::open(&db_path).await.expect("open db"));
    let engine = StatsEngine::new(Arc::clone(&db));
    (engine, db, dir)
}

fn make_tool_call_event(tool: &str, ts: &str, success: bool, duration_ms: Option<u64>) -> LogEvent {
    LogEvent {
        ts: Some(ts.to_owned()),
        level: Some("info".to_owned()),
        event: Some("tool_call".to_owned()),
        tool: Some(tool.to_owned()),
        duration_ms,
        success: Some(success),
        error: if success {
            None
        } else {
            Some("test error".to_owned())
        },
    }
}

#[tokio::test]
async fn ingest_tool_call_events() {
    let (engine, db, _dir) = setup().await;

    let events = vec![
        make_tool_call_event("news_digest", "2026-02-19T14:30:00Z", true, Some(1200)),
        make_tool_call_event("news_digest", "2026-02-19T14:45:00Z", false, Some(3000)),
        make_tool_call_event("news_digest", "2026-02-19T15:10:00Z", true, Some(800)),
        make_tool_call_event("deploy_check", "2026-02-19T14:30:00Z", false, Some(30000)),
    ];

    engine.ingest(&events).await.expect("ingest");

    // Check news_digest hour 14: 1 success + 1 failure.
    let stats = db
        .tool_stats("news_digest", "2026-02-19T14:00:00+00:00")
        .await
        .expect("query");

    assert!(!stats.is_empty());
    let hour_14 = stats
        .iter()
        .find(|s| s.window_start.contains("14:00:00"))
        .expect("hour 14 bucket");
    assert_eq!(hour_14.success_count, 1);
    assert_eq!(hour_14.failure_count, 1);
}

#[tokio::test]
async fn ingest_ignores_non_tool_call_events() {
    let (engine, db, _dir) = setup().await;

    let events = vec![LogEvent {
        ts: Some("2026-02-19T14:30:00Z".to_owned()),
        level: Some("warn".to_owned()),
        event: Some("budget".to_owned()),
        tool: None,
        duration_ms: None,
        success: None,
        error: None,
    }];

    engine.ingest(&events).await.expect("ingest");

    // No tool stats should be recorded.
    let names = db
        .distinct_tool_names("2020-01-01T00:00:00Z")
        .await
        .expect("names");
    assert!(names.is_empty());
}

#[tokio::test]
async fn tool_failure_rate_calculation() {
    let (engine, db, _dir) = setup().await;

    // Use a recent timestamp so hours_ago(24) includes it.
    let bucket = recent_bucket();
    for _ in 0..3 {
        db.record_tool_stat("flaky_tool", &bucket, true, None)
            .await
            .expect("record");
    }
    for _ in 0..7 {
        db.record_tool_stat("flaky_tool", &bucket, false, None)
            .await
            .expect("record");
    }

    let rate = engine
        .tool_failure_rate("flaky_tool", 24)
        .await
        .expect("rate");

    // 7 failures / 10 total = 0.7.
    assert!((rate - 0.7).abs() < 0.01);
}

#[tokio::test]
async fn tool_failure_rate_zero_for_unknown_tool() {
    let (engine, _db, _dir) = setup().await;

    let rate = engine
        .tool_failure_rate("nonexistent", 24)
        .await
        .expect("rate");

    assert!((rate - 0.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn failing_tools_filters_by_threshold() {
    let (engine, db, _dir) = setup().await;

    let bucket = recent_bucket();

    // healthy_tool: 9 success, 1 failure (10% failure rate).
    for _ in 0..9 {
        db.record_tool_stat("healthy_tool", &bucket, true, None)
            .await
            .expect("record");
    }
    db.record_tool_stat("healthy_tool", &bucket, false, None)
        .await
        .expect("record");

    // broken_tool: 1 success, 9 failures (90% failure rate).
    db.record_tool_stat("broken_tool", &bucket, true, None)
        .await
        .expect("record");
    for _ in 0..9 {
        db.record_tool_stat("broken_tool", &bucket, false, None)
            .await
            .expect("record");
    }

    // Threshold 0.5: only broken_tool should appear.
    let failing = engine.failing_tools(0.5, 24).await.expect("failing tools");

    assert_eq!(failing.len(), 1);
    assert_eq!(failing[0].0, "broken_tool");
    assert!((failing[0].1 - 0.9).abs() < 0.01);
}

#[tokio::test]
async fn budget_burn_rate_zero_limit() {
    let (engine, _db, _dir) = setup().await;

    let health = make_health_report(0, 0);
    let rate = engine.budget_burn_rate(&health).await;
    assert!((rate - 0.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn budget_burn_rate_positive() {
    let (engine, _db, _dir) = setup().await;

    let health = make_health_report(50000, 100000);
    let rate = engine.budget_burn_rate(&health).await;

    // Budget is 50% used. Burn rate depends on time of day.
    // It should be > 0 since some of the day has elapsed.
    assert!(rate > 0.0);
}

fn make_health_report(used: u64, limit: u64) -> HealthReport {
    HealthReport {
        status: "running".to_owned(),
        uptime_secs: 86400,
        last_heartbeat: chrono::Utc::now().to_rfc3339(),
        executor: "docker".to_owned(),
        container_healthy: true,
        active_sessions: 0,
        memory_db_size_mb: 0.0,
        scripts_count: 0,
        dynamic_tools_count: 0,
        budget_today: BudgetReport { used, limit },
        last_error: None,
    }
}
