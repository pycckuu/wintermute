//! Tests for `src/tools/browser.rs` — browser input validation and bridge behaviors.

use async_trait::async_trait;
use serde_json::json;

use wintermute::agent::policy::RateLimiter;
use wintermute::config::BrowserConfig;
use wintermute::tools::browser::{
    browser_tool_definition, detect_browser, run_browser, validate_browser_input, BrowserBridge,
    BrowserMode, SIDECAR_PORT,
};

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
        "list_tabs",
        "switch_tab",
        "new_tab",
        "close_tab",
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
    let input = json!({"action": "screenshot"});

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

// ---------------------------------------------------------------------------
// Tab management tests
// ---------------------------------------------------------------------------

#[test]
fn validate_browser_input_rejects_close_action() {
    let input = json!({"action": "close"});
    let result = validate_browser_input(&input);
    assert!(result.is_err());
    assert!(
        result
            .expect_err("should fail")
            .to_string()
            .contains("invalid action"),
        "close should no longer be a valid action"
    );
}

#[test]
fn validate_browser_input_accepts_tab_management_actions() {
    for action in ["list_tabs", "switch_tab", "new_tab", "close_tab"] {
        let input = json!({"action": action});
        let result = validate_browser_input(&input);
        assert!(
            result.is_ok(),
            "tab management action {action} should be valid"
        );
    }
}

#[test]
fn validate_browser_input_accepts_tab_id_parameter() {
    let input = json!({
        "action": "switch_tab",
        "tab_id": "page-abc-123"
    });
    let result = validate_browser_input(&input);
    assert!(result.is_ok());
    let sanitised = result.expect("should succeed");
    assert_eq!(
        sanitised.get("tab_id").and_then(|v| v.as_str()),
        Some("page-abc-123"),
        "tab_id should be preserved in sanitised output"
    );
}

#[test]
fn browser_tool_definition_includes_tab_actions() {
    let def = browser_tool_definition();
    let schema = &def.input_schema;
    let action_enum = schema
        .pointer("/properties/action/enum")
        .expect("action enum should exist");
    let actions: Vec<&str> = action_enum
        .as_array()
        .expect("enum should be an array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();

    // Tab actions are present
    for expected in ["list_tabs", "switch_tab", "new_tab", "close_tab"] {
        assert!(
            actions.contains(&expected),
            "tool definition should include {expected} in action enum"
        );
    }

    // "close" is NOT present
    assert!(
        !actions.contains(&"close"),
        "tool definition should not include 'close' in action enum"
    );

    // tab_id property exists
    let tab_id_prop = schema.pointer("/properties/tab_id");
    assert!(
        tab_id_prop.is_some(),
        "tool definition should include tab_id property"
    );
    assert_eq!(
        tab_id_prop
            .expect("tab_id should exist")
            .get("type")
            .and_then(|v| v.as_str()),
        Some("string"),
        "tab_id should be a string type"
    );
}

// ---------------------------------------------------------------------------
// Browser mode detection tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn detect_browser_returns_standalone_when_fallback_enabled() {
    let config = BrowserConfig::default(); // standalone_fallback defaults to true
    let mode = detect_browser(&config).await;
    assert_eq!(mode, BrowserMode::Standalone { port: SIDECAR_PORT });
}

#[tokio::test]
async fn detect_browser_returns_none_when_fallback_disabled() {
    let config = BrowserConfig {
        standalone_fallback: false,
        ..BrowserConfig::default()
    };
    let mode = detect_browser(&config).await;
    assert_eq!(mode, BrowserMode::None);
}

#[test]
fn browser_mode_debug_and_clone() {
    let mode = BrowserMode::Attached { port: 9222 };
    let cloned = mode;
    assert_eq!(mode, cloned);
    assert_eq!(format!("{mode:?}"), "Attached { port: 9222 }");
}
