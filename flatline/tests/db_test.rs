//! Tests for the Flatline state database.

use flatline::db::{FixRecord, StateDb, UpdateRecord};

async fn open_temp_db() -> (StateDb, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("test_state.db");
    let db = StateDb::open(&db_path).await.expect("open db");
    (db, dir)
}

#[tokio::test]
async fn open_creates_tables() {
    let (db, _dir) = open_temp_db().await;

    // Verify we can interact with all three tables without errors.
    let stats = db.tool_stats("nonexistent", "2020-01-01T00:00:00Z").await;
    assert!(stats.is_ok());
    assert!(stats.expect("stats").is_empty());

    let fixes = db.recent_fixes(10).await;
    assert!(fixes.is_ok());
    assert!(fixes.expect("fixes").is_empty());

    let suppressed = db.is_suppressed("nonexistent").await;
    assert!(suppressed.is_ok());
    assert!(!suppressed.expect("suppressed"));
}

#[tokio::test]
async fn record_tool_stat_and_query_roundtrip() {
    let (db, _dir) = open_temp_db().await;

    // Record some stats.
    db.record_tool_stat("news_digest", "2026-02-19T14:00:00+00:00", true, Some(1200))
        .await
        .expect("record 1");
    db.record_tool_stat(
        "news_digest",
        "2026-02-19T14:00:00+00:00",
        false,
        Some(3000),
    )
    .await
    .expect("record 2");
    db.record_tool_stat("news_digest", "2026-02-19T15:00:00+00:00", true, Some(800))
        .await
        .expect("record 3");

    // Query all stats since start of day.
    let rows = db
        .tool_stats("news_digest", "2026-02-19T00:00:00+00:00")
        .await
        .expect("query stats");

    assert_eq!(rows.len(), 2);

    // First bucket: 1 success + 1 failure.
    assert_eq!(rows[0].window_start, "2026-02-19T14:00:00+00:00");
    assert_eq!(rows[0].success_count, 1);
    assert_eq!(rows[0].failure_count, 1);

    // Second bucket: 1 success, 0 failures.
    assert_eq!(rows[1].window_start, "2026-02-19T15:00:00+00:00");
    assert_eq!(rows[1].success_count, 1);
    assert_eq!(rows[1].failure_count, 0);
}

#[tokio::test]
async fn record_tool_stat_without_duration() {
    let (db, _dir) = open_temp_db().await;

    db.record_tool_stat("test_tool", "2026-01-01T00:00:00+00:00", true, None)
        .await
        .expect("record without duration");

    let rows = db
        .tool_stats("test_tool", "2026-01-01T00:00:00+00:00")
        .await
        .expect("query");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].success_count, 1);
    assert!(rows[0].avg_duration_ms.is_none());
}

#[tokio::test]
async fn insert_fix_and_recent_fixes() {
    let (db, _dir) = open_temp_db().await;

    let fix = FixRecord {
        id: "fix-001".to_owned(),
        detected_at: "2026-02-19T14:05:00Z".to_owned(),
        pattern: Some("tool_failing_after_change".to_owned()),
        diagnosis: Some("deploy_check failing after commit".to_owned()),
        action: Some("quarantine_and_revert".to_owned()),
        applied_at: Some("2026-02-19T14:06:00Z".to_owned()),
        verified: Some(true),
        user_notified: true,
    };

    db.insert_fix(&fix).await.expect("insert fix");

    let fixes = db.recent_fixes(10).await.expect("recent fixes");
    assert_eq!(fixes.len(), 1);
    assert_eq!(fixes[0].id, "fix-001");
    assert_eq!(
        fixes[0].pattern.as_deref(),
        Some("tool_failing_after_change")
    );
    assert_eq!(fixes[0].verified, Some(true));
    assert!(fixes[0].user_notified);
}

#[tokio::test]
async fn update_fix_updates_fields() {
    let (db, _dir) = open_temp_db().await;

    let fix = FixRecord {
        id: "fix-002".to_owned(),
        detected_at: "2026-02-19T15:00:00Z".to_owned(),
        pattern: None,
        diagnosis: None,
        action: None,
        applied_at: None,
        verified: None,
        user_notified: false,
    };

    db.insert_fix(&fix).await.expect("insert");

    db.update_fix(
        "fix-002",
        Some("2026-02-19T15:05:00Z"),
        Some(true),
        Some(true),
    )
    .await
    .expect("update");

    let fixes = db.recent_fixes(10).await.expect("query");
    assert_eq!(fixes.len(), 1);
    assert_eq!(fixes[0].applied_at.as_deref(), Some("2026-02-19T15:05:00Z"));
    assert_eq!(fixes[0].verified, Some(true));
    assert!(fixes[0].user_notified);
}

#[tokio::test]
async fn suppress_and_is_suppressed() {
    let (db, _dir) = open_temp_db().await;

    // Not suppressed initially.
    assert!(!db.is_suppressed("tool_sprawl").await.expect("check 1"));

    // Suppress with a far-future expiry.
    db.suppress(
        "tool_sprawl",
        Some("2099-12-31T23:59:59Z"),
        Some("user asked to ignore"),
    )
    .await
    .expect("suppress");

    assert!(db.is_suppressed("tool_sprawl").await.expect("check 2"));

    // Different pattern should not be suppressed.
    assert!(!db.is_suppressed("budget_burn").await.expect("check 3"));
}

#[tokio::test]
async fn suppress_with_expired_time_not_suppressed() {
    let (db, _dir) = open_temp_db().await;

    // Suppress with a past expiry.
    db.suppress("old_pattern", Some("2020-01-01T00:00:00Z"), Some("expired"))
        .await
        .expect("suppress");

    // Should not be suppressed because the time has passed.
    assert!(!db.is_suppressed("old_pattern").await.expect("check"));
}

#[tokio::test]
async fn suppress_without_expiry_is_permanent() {
    let (db, _dir) = open_temp_db().await;

    db.suppress("permanent_pattern", None, Some("always suppress"))
        .await
        .expect("suppress");

    assert!(db.is_suppressed("permanent_pattern").await.expect("check"));
}

#[tokio::test]
async fn distinct_tool_names() {
    let (db, _dir) = open_temp_db().await;

    db.record_tool_stat("alpha", "2026-01-01T00:00:00+00:00", true, None)
        .await
        .expect("record");
    db.record_tool_stat("beta", "2026-01-01T00:00:00+00:00", false, None)
        .await
        .expect("record");
    db.record_tool_stat("alpha", "2026-01-01T01:00:00+00:00", true, None)
        .await
        .expect("record");

    let names = db
        .distinct_tool_names("2026-01-01T00:00:00+00:00")
        .await
        .expect("names");

    assert_eq!(names.len(), 2);
    assert!(names.contains(&"alpha".to_owned()));
    assert!(names.contains(&"beta".to_owned()));
}

// -- Update record tests --

fn make_update_record(to_version: &str, status: &str) -> UpdateRecord {
    UpdateRecord {
        id: 0,
        checked_at: "2026-02-19T04:00:00Z".to_owned(),
        from_version: "0.1.0".to_owned(),
        to_version: to_version.to_owned(),
        status: status.to_owned(),
        started_at: None,
        completed_at: None,
        rollback_reason: None,
        migration_log: None,
    }
}

#[tokio::test]
async fn insert_and_query_update_record() {
    let (db, _dir) = open_temp_db().await;

    let record = make_update_record("0.2.0", "pending");
    let id = db.insert_update(&record).await.expect("insert update");
    assert!(id > 0);

    let latest = db.latest_update().await.expect("latest").expect("some");
    assert_eq!(latest.id, id);
    assert_eq!(latest.from_version, "0.1.0");
    assert_eq!(latest.to_version, "0.2.0");
    assert_eq!(latest.status, "pending");
}

#[tokio::test]
async fn update_status_transition() {
    let (db, _dir) = open_temp_db().await;

    let record = make_update_record("0.3.0", "pending");
    let id = db.insert_update(&record).await.expect("insert");

    // Transition to downloading.
    db.set_update_status(id, "downloading", None, None, None, None)
        .await
        .expect("update to downloading");

    let r = db.latest_update().await.expect("query").expect("some");
    assert_eq!(r.status, "downloading");

    // Transition to applying.
    db.set_update_status(
        id,
        "applying",
        Some("2026-02-19T04:05:00Z"),
        None,
        None,
        None,
    )
    .await
    .expect("update to applying");

    let r = db.latest_update().await.expect("query").expect("some");
    assert_eq!(r.status, "applying");
    assert_eq!(r.started_at.as_deref(), Some("2026-02-19T04:05:00Z"));

    // Transition to healthy.
    db.set_update_status(
        id,
        "healthy",
        None,
        Some("2026-02-19T04:10:00Z"),
        None,
        None,
    )
    .await
    .expect("update to healthy");

    let r = db.latest_update().await.expect("query").expect("some");
    assert_eq!(r.status, "healthy");
    assert_eq!(r.completed_at.as_deref(), Some("2026-02-19T04:10:00Z"));
}

#[tokio::test]
async fn pending_update_returns_active_record() {
    let (db, _dir) = open_temp_db().await;

    // No pending updates initially.
    let pending = db.pending_update().await.expect("query");
    assert!(pending.is_none());

    // Insert a pending update.
    let record = make_update_record("0.4.0", "pending");
    db.insert_update(&record).await.expect("insert");

    let pending = db.pending_update().await.expect("query").expect("some");
    assert_eq!(pending.to_version, "0.4.0");

    // Insert a healthy (completed) update — should not be returned by pending.
    let mut record2 = make_update_record("0.5.0", "healthy");
    record2.completed_at = Some("2026-02-20T04:00:00Z".to_owned());
    db.insert_update(&record2).await.expect("insert 2");

    let pending = db.pending_update().await.expect("query").expect("some");
    // Should still find the pending 0.4.0, not the healthy 0.5.0.
    assert_eq!(pending.to_version, "0.4.0");
}

#[tokio::test]
async fn latest_update_returns_most_recent() {
    let (db, _dir) = open_temp_db().await;

    let r1 = make_update_record("0.2.0", "healthy");
    db.insert_update(&r1).await.expect("insert 1");

    let r2 = make_update_record("0.3.0", "pending");
    db.insert_update(&r2).await.expect("insert 2");

    let latest = db.latest_update().await.expect("query").expect("some");
    assert_eq!(latest.to_version, "0.3.0");
}

#[tokio::test]
async fn update_rollback_records_reason() {
    let (db, _dir) = open_temp_db().await;

    let record = make_update_record("0.6.0", "applying");
    let id = db.insert_update(&record).await.expect("insert");

    db.set_update_status(
        id,
        "rolled_back",
        None,
        Some("2026-02-19T04:15:00Z"),
        Some("health checks failed"),
        Some("migration: ok"),
    )
    .await
    .expect("rollback update");

    let r = db.latest_update().await.expect("query").expect("some");
    assert_eq!(r.status, "rolled_back");
    assert_eq!(r.rollback_reason.as_deref(), Some("health checks failed"));
    assert_eq!(r.migration_log.as_deref(), Some("migration: ok"));
}
