//! Tests for `src/tools/browser.rs` — browser input validation and bridge behaviors.

use async_trait::async_trait;
use serde_json::json;

use wintermute::agent::policy::RateLimiter;
use wintermute::tools::browser::{run_browser, validate_browser_input, BrowserBridge};

// ---------------------------------------------------------------------------
// Input validation tests
// ---------------------------------------------------------------------------

#[test]
fn validate_browser_input_requires_action() {
    let input = json!({});
    let result = validate_browser_input(&input);
    assert!(result.is_err());
    assert!(result
        .expect_err("should fail")
        .to_string()
        .contains("action"));
}

#[test]
fn validate_browser_input_rejects_invalid_action() {
    let input = json!({"action": "invalid_action"});
    let result = validate_browser_input(&input);
    assert!(result.is_err());
    assert!(result
        .expect_err("should fail")
        .to_string()
        .contains("invalid action"));
}

#[test]
fn validate_browser_input_accepts_valid_actions() {
    for action in [
        "navigate",
        "click",
        "type",
        "screenshot",
        "extract",
        "wait",
        "scroll",
        "evaluate",
        "close",
    ] {
        let input = json!({"action": action});
        let result = validate_browser_input(&input);
        assert!(result.is_ok(), "action {action} should be valid");
    }
}

#[test]
fn validate_browser_input_rejects_excessive_timeout() {
    let input = json!({
        "action": "navigate",
        "timeout_ms": 200_000
    });
    let result = validate_browser_input(&input);
    assert!(result.is_err());
    assert!(result
        .expect_err("should fail")
        .to_string()
        .contains("timeout"));
}

#[test]
fn validate_browser_input_sanitises_string_params() {
    let input = json!({
        "action": "navigate",
        "url": "https://example.com",
        "timeout_ms": 5000
    });
    let result = validate_browser_input(&input);
    assert!(result.is_ok());
    let sanitised = result.expect("should succeed");
    assert_eq!(
        sanitised.get("action").and_then(|v| v.as_str()),
        Some("navigate")
    );
    assert_eq!(
        sanitised.get("url").and_then(|v| v.as_str()),
        Some("https://example.com")
    );
    assert_eq!(
        sanitised.get("timeout_ms").and_then(|v| v.as_u64()),
        Some(5000)
    );
}

#[test]
fn validate_browser_input_rejects_excessive_url_length() {
    let long_url = "https://example.com/".to_owned() + &"a".repeat(20_000);
    let input = json!({
        "action": "navigate",
        "url": long_url
    });
    let result = validate_browser_input(&input);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Bridge unavailable tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_browser_returns_unavailable_when_no_bridge() {
    let limiter = RateLimiter::new(60, 60);
    let input = json!({"action": "navigate", "url": "https://example.com"});

    let result = run_browser(&input, &limiter, None).await;

    assert!(result.is_err());
    let err = result.expect_err("should fail");
    assert!(
        err.to_string().contains("unavailable") || err.to_string().contains("no runtime bridge"),
        "error should indicate bridge unavailable: {}",
        err
    );
}

#[tokio::test]
async fn run_browser_rate_limited() {
    let limiter = RateLimiter::new(60, 0);
    let input = json!({"action": "close"});

    let result = run_browser(&input, &limiter, None).await;

    assert!(result.is_err());
    assert!(result
        .expect_err("should fail")
        .to_string()
        .contains("rate"));
}

struct BridgeNeverCalled;

#[async_trait]
impl BrowserBridge for BridgeNeverCalled {
    async fn execute(&self, _action: &str, _input: &serde_json::Value) -> Result<String, String> {
        Ok("ok".to_owned())
    }
}

#[tokio::test]
async fn run_browser_navigate_blocks_private_ip_ssrf() {
    let limiter = RateLimiter::new(60, 60);
    let bridge = BridgeNeverCalled;
    let input = json!({"action": "navigate", "url": "http://127.0.0.1/private"});

    let result = run_browser(&input, &limiter, Some(&bridge)).await;

    assert!(result.is_err());
    assert!(
        result
            .expect_err("should fail")
            .to_string()
            .contains("SSRF blocked"),
        "navigate to private IP should be blocked before bridge execution"
    );
}
