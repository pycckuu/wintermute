//! Tests for `src/heartbeat/tool_review.rs` — monthly tool health review.

use serde_json::json;
use tempfile::TempDir;
use tokio::sync::mpsc;

use std::sync::Arc;

use wintermute::agent::TelegramOutbound;
use wintermute::heartbeat::tool_review::execute_tool_review;
use wintermute::tools::registry::DynamicToolRegistry;

fn setup_registry_with_tools(dir: &TempDir) -> Arc<DynamicToolRegistry> {
    let path = dir.path().to_path_buf();

    // Tool with good health.
    let good = json!({
        "name": "good_tool",
        "description": "Works well",
        "parameters": { "type": "object" },
        "_meta": {
            "created_at": "2025-01-01T00:00:00Z",
            "last_used": chrono::Utc::now().to_rfc3339(),
            "invocations": 100,
            "success_rate": 0.95,
            "avg_duration_ms": 500,
            "last_error": null,
            "version": 1
        }
    });
    std::fs::write(
        path.join("good_tool.json"),
        serde_json::to_string_pretty(&good).expect("json"),
    )
    .expect("write");

    // Unused tool (last_used 60 days ago).
    let unused = json!({
        "name": "unused_tool",
        "description": "Never used recently",
        "parameters": { "type": "object" },
        "_meta": {
            "created_at": "2024-06-01T00:00:00Z",
            "last_used": "2024-12-01T00:00:00Z",
            "invocations": 2,
            "success_rate": 1.0,
            "avg_duration_ms": 100,
            "last_error": null,
            "version": 1
        }
    });
    std::fs::write(
        path.join("unused_tool.json"),
        serde_json::to_string_pretty(&unused).expect("json"),
    )
    .expect("write");

    // Failing tool.
    let failing = json!({
        "name": "failing_tool",
        "description": "Often fails",
        "parameters": { "type": "object" },
        "_meta": {
            "created_at": "2025-01-01T00:00:00Z",
            "last_used": chrono::Utc::now().to_rfc3339(),
            "invocations": 20,
            "success_rate": 0.50,
            "avg_duration_ms": 1000,
            "last_error": "timeout",
            "version": 1
        }
    });
    std::fs::write(
        path.join("failing_tool.json"),
        serde_json::to_string_pretty(&failing).expect("json"),
    )
    .expect("write");

    // Slow tool.
    let slow = json!({
        "name": "slow_tool",
        "description": "Very slow",
        "parameters": { "type": "object" },
        "_meta": {
            "created_at": "2025-01-01T00:00:00Z",
            "last_used": chrono::Utc::now().to_rfc3339(),
            "invocations": 10,
            "success_rate": 0.90,
            "avg_duration_ms": 15000,
            "last_error": null,
            "version": 1
        }
    });
    std::fs::write(
        path.join("slow_tool.json"),
        serde_json::to_string_pretty(&slow).expect("json"),
    )
    .expect("write");

    DynamicToolRegistry::new_without_watcher(path).expect("registry")
}

#[tokio::test]
async fn tool_review_detects_unused_tools() {
    let dir = TempDir::new().expect("temp dir");
    let registry = setup_registry_with_tools(&dir);
    let (tx, mut rx) = mpsc::channel::<TelegramOutbound>(16);

    let report = execute_tool_review(&registry, &tx, 12345)
        .await
        .expect("should succeed");

    assert!(report.contains("unused_tool"), "should detect unused tool");

    // Verify message was sent.
    let msg = rx.try_recv().expect("should receive message");
    assert_eq!(msg.user_id, 12345);
    assert!(msg.text.is_some());
}

#[tokio::test]
async fn tool_review_detects_failing_tools() {
    let dir = TempDir::new().expect("temp dir");
    let registry = setup_registry_with_tools(&dir);
    let (tx, _rx) = mpsc::channel::<TelegramOutbound>(16);

    let report = execute_tool_review(&registry, &tx, 12345)
        .await
        .expect("should succeed");

    assert!(
        report.contains("failing_tool"),
        "should detect failing tool"
    );
    assert!(report.contains("50%"), "should show success rate");
}

#[tokio::test]
async fn tool_review_detects_slow_tools() {
    let dir = TempDir::new().expect("temp dir");
    let registry = setup_registry_with_tools(&dir);
    let (tx, _rx) = mpsc::channel::<TelegramOutbound>(16);

    let report = execute_tool_review(&registry, &tx, 12345)
        .await
        .expect("should succeed");

    assert!(report.contains("slow_tool"), "should detect slow tool");
}

#[tokio::test]
async fn tool_review_empty_registry_shows_healthy() {
    let dir = TempDir::new().expect("temp dir");
    let registry =
        DynamicToolRegistry::new_without_watcher(dir.path().to_path_buf()).expect("registry");
    let (tx, _rx) = mpsc::channel::<TelegramOutbound>(16);

    let report = execute_tool_review(&registry, &tx, 12345)
        .await
        .expect("should succeed");

    assert!(
        report.contains("All tools are healthy"),
        "empty registry should be healthy"
    );
}
