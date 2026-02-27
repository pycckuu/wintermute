//! Tests for `src/tools/flatline.rs` — Flatline supervisor status tool.

use std::path::PathBuf;

use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::TempDir;

use wintermute::tools::flatline::{flatline_status, flatline_status_tool_definition};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a temp dir with a `state.db` populated with the Flatline schema.
async fn make_flatline_root() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    let root = dir.path().to_path_buf();

    let db_path = root.join("state.db");
    let opts = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("pool should connect");

    let schema = include_str!("../../flatline/migrations/001_flatline_schema.sql");
    sqlx::raw_sql(schema)
        .execute(&pool)
        .await
        .expect("schema should apply");

    pool.close().await;

    (dir, root)
}

/// Insert an update row into state.db.
async fn insert_update(root: &std::path::Path, from: &str, to: &str, status: &str) {
    let db_path = root.join("state.db");
    let opts = SqliteConnectOptions::new().filename(&db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("connect");

    sqlx::query(
        "INSERT INTO updates (checked_at, from_version, to_version, status) \
         VALUES (datetime('now'), ?1, ?2, ?3)",
    )
    .bind(from)
    .bind(to)
    .bind(status)
    .execute(&pool)
    .await
    .expect("insert update");

    pool.close().await;
}

/// Insert a fix row into state.db.
async fn insert_fix(root: &std::path::Path, id: &str, pattern: &str, action: &str) {
    let db_path = root.join("state.db");
    let opts = SqliteConnectOptions::new().filename(&db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("connect");

    sqlx::query(
        "INSERT INTO fixes (id, detected_at, pattern, action) \
         VALUES (?1, datetime('now'), ?2, ?3)",
    )
    .bind(id)
    .bind(pattern)
    .bind(action)
    .execute(&pool)
    .await
    .expect("insert fix");

    pool.close().await;
}

/// Insert a tool_stats row into state.db.
async fn insert_tool_stat(
    root: &std::path::Path,
    tool_name: &str,
    window_start: &str,
    success: i64,
    failure: i64,
) {
    let db_path = root.join("state.db");
    let opts = SqliteConnectOptions::new().filename(&db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("connect");

    sqlx::query(
        "INSERT INTO tool_stats (tool_name, window_start, success_count, failure_count) \
         VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(tool_name)
    .bind(window_start)
    .bind(success)
    .bind(failure)
    .execute(&pool)
    .await
    .expect("insert tool_stat");

    pool.close().await;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn definition_has_correct_name() {
    let def = flatline_status_tool_definition();
    assert_eq!(def.name, "flatline_status");
    assert!(
        def.description.contains("Flatline"),
        "description should mention Flatline"
    );
    // Schema should have section and limit properties.
    let props = def.input_schema.get("properties").expect("properties");
    assert!(props.get("section").is_some());
    assert!(props.get("limit").is_some());
}

#[tokio::test]
async fn root_not_found_returns_error() {
    let nonexistent = PathBuf::from("/tmp/flatline_test_nonexistent_dir_xyz");
    let result = flatline_status(&nonexistent, &json!({})).await;
    assert!(result.is_err());
    let err = result.expect_err("should return error").to_string();
    assert!(
        err.contains("not installed"),
        "error should indicate not installed, got: {err}"
    );
}

#[tokio::test]
async fn summary_empty_db() {
    let (_dir, root) = make_flatline_root().await;

    let result = flatline_status(&root, &json!({}))
        .await
        .expect("summary should succeed on empty db");

    let parsed: serde_json::Value = serde_json::from_str(&result).expect("should be valid JSON");
    assert!(parsed["latest_update"].is_null());
    assert_eq!(parsed["recent_fixes"].as_array().expect("array").len(), 0);
    assert_eq!(
        parsed["active_suppressions"]
            .as_array()
            .expect("array")
            .len(),
        0
    );
}

#[tokio::test]
async fn summary_with_update_and_fix() {
    let (_dir, root) = make_flatline_root().await;

    insert_update(&root, "0.4.0", "0.5.0", "completed").await;
    insert_fix(&root, "fix-001", "crash_loop", "restart").await;

    let result = flatline_status(&root, &json!({}))
        .await
        .expect("summary should succeed");

    let parsed: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
    assert!(!parsed["latest_update"].is_null());
    assert_eq!(parsed["latest_update"]["from_version"], "0.4.0");
    assert_eq!(parsed["latest_update"]["to_version"], "0.5.0");
    assert_eq!(parsed["latest_update"]["status"], "completed");

    let fixes = parsed["recent_fixes"].as_array().expect("array");
    assert_eq!(fixes.len(), 1);
    assert_eq!(fixes[0]["id"], "fix-001");
    assert_eq!(fixes[0]["pattern"], "crash_loop");
}

#[tokio::test]
async fn section_updates() {
    let (_dir, root) = make_flatline_root().await;

    insert_update(&root, "0.3.0", "0.4.0", "completed").await;
    insert_update(&root, "0.4.0", "0.5.0", "completed").await;
    insert_update(&root, "0.5.0", "0.6.0", "pending").await;

    let result = flatline_status(&root, &json!({"section": "updates"}))
        .await
        .expect("updates section should succeed");

    let parsed: Vec<serde_json::Value> = serde_json::from_str(&result).expect("valid JSON array");
    assert_eq!(parsed.len(), 3);
    // Most recent first.
    assert_eq!(parsed[0]["to_version"], "0.6.0");
    assert_eq!(parsed[2]["to_version"], "0.4.0");
}

#[tokio::test]
async fn section_fixes() {
    let (_dir, root) = make_flatline_root().await;

    insert_fix(&root, "fix-a", "tool_failure", "quarantine").await;
    insert_fix(&root, "fix-b", "crash_loop", "restart").await;

    let result = flatline_status(&root, &json!({"section": "fixes"}))
        .await
        .expect("fixes section should succeed");

    let parsed: Vec<serde_json::Value> = serde_json::from_str(&result).expect("valid JSON array");
    assert_eq!(parsed.len(), 2);
}

#[tokio::test]
async fn section_stats_filters_by_time() {
    let (_dir, root) = make_flatline_root().await;

    // Recent stat (within 24h).
    insert_tool_stat(
        &root,
        "execute_command",
        &chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        10,
        2,
    )
    .await;

    // Old stat (beyond 24h window).
    insert_tool_stat(&root, "web_fetch", "2020-01-01 00:00:00", 100, 50).await;

    let result = flatline_status(&root, &json!({"section": "stats"}))
        .await
        .expect("stats section should succeed");

    let parsed: Vec<serde_json::Value> = serde_json::from_str(&result).expect("valid JSON array");
    // Only the recent stat should appear.
    assert_eq!(parsed.len(), 1, "only recent stats should be returned");
    assert_eq!(parsed[0]["tool_name"], "execute_command");
}

#[tokio::test]
async fn section_logs_reads_recent() {
    let (_dir, root) = make_flatline_root().await;

    let logs_dir = root.join("logs");
    std::fs::create_dir_all(&logs_dir).expect("create logs dir");

    // Write some log lines.
    let log_file = logs_dir.join("wintermute.log.2026-02-27.jsonl");
    let lines: Vec<String> = (0..5)
        .map(|i| format!("{{\"level\":\"INFO\",\"msg\":\"event {i}\"}}"))
        .collect();
    std::fs::write(&log_file, lines.join("\n")).expect("write log");

    let result = flatline_status(&root, &json!({"section": "logs"}))
        .await
        .expect("logs section should succeed");

    assert!(result.contains("event 0"));
    assert!(result.contains("event 4"));
}

#[tokio::test]
async fn section_logs_respects_limit() {
    let (_dir, root) = make_flatline_root().await;

    let logs_dir = root.join("logs");
    std::fs::create_dir_all(&logs_dir).expect("create logs dir");

    let log_file = logs_dir.join("flatline.jsonl");
    let lines: Vec<String> = (0..10)
        .map(|i| format!("{{\"level\":\"INFO\",\"msg\":\"line {i}\"}}"))
        .collect();
    std::fs::write(&log_file, lines.join("\n")).expect("write log");

    let result = flatline_status(&root, &json!({"section": "logs", "limit": 3}))
        .await
        .expect("logs with limit should succeed");

    let output_lines: Vec<&str> = result.lines().collect();
    assert_eq!(
        output_lines.len(),
        3,
        "should return exactly 3 lines, got {}",
        output_lines.len()
    );
    // Should be the last 3 lines.
    assert!(result.contains("line 7"));
    assert!(result.contains("line 8"));
    assert!(result.contains("line 9"));
}

#[tokio::test]
async fn unknown_section_returns_error() {
    let (_dir, root) = make_flatline_root().await;

    let result = flatline_status(&root, &json!({"section": "bogus"})).await;
    assert!(result.is_err());
    let err = result.expect_err("should return error").to_string();
    assert!(
        err.contains("unknown section"),
        "error should mention unknown section, got: {err}"
    );
}
