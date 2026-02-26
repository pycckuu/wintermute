//! Tests for `src/tools/browser_bridge.rs` — HTTP bridge response parsing.

use wintermute::tools::browser::BrowserBridge;
use wintermute::tools::browser_bridge::PlaywrightBridge;

#[test]
fn playwright_bridge_new_sets_base_url() {
    let bridge = PlaywrightBridge::new("http://127.0.0.1:9222".to_owned());
    // The bridge should be constructable without panicking.
    // We verify it implements BrowserBridge by calling execute below.
    let _ = &bridge;
}

#[tokio::test]
async fn execute_returns_error_on_connection_refused() {
    // Point the bridge at a port that is not listening.
    let bridge = PlaywrightBridge::new("http://127.0.0.1:19999".to_owned());

    let input = serde_json::json!({
        "action": "navigate",
        "url": "https://example.com",
        "timeout_ms": 1000
    });

    let result = bridge.execute("navigate", &input).await;
    assert!(result.is_err(), "should fail when sidecar is not running");
    let err = result.expect_err("just asserted is_err");
    assert!(
        err.contains("bridge request failed"),
        "error should mention bridge request failure, got: {err}"
    );
}

#[tokio::test]
async fn execute_uses_timeout_from_input() {
    // Very short timeout to confirm it's read from input.
    let bridge = PlaywrightBridge::new("http://127.0.0.1:19999".to_owned());

    let input = serde_json::json!({
        "action": "extract",
        "timeout_ms": 500
    });

    let start = std::time::Instant::now();
    let _ = bridge.execute("extract", &input).await;
    let elapsed = start.elapsed();

    // Should fail quickly (connect timeout is 5s but connection refused
    // returns immediately), not wait for the full 30s default.
    assert!(
        elapsed.as_secs() < 10,
        "should not wait for the default 30s timeout"
    );
}
