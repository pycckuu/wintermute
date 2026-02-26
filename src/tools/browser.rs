//! Browser tool: safe bridge interface for Playwright-style automation.
//!
//! This module provides input validation, rate limiting, and a bridge interface
//! that returns a clear unavailable error when no runtime bridge is configured.
//! No host process-command API is used here — the actual
//! browser automation is delegated to an optional external bridge (e.g. MCP or
//! future subprocess integration) configured at runtime.

use async_trait::async_trait;
use serde_json::json;
use tracing::debug;
use url::Url;

use crate::agent::policy::{ssrf_check, RateLimiter};
use crate::executor::playwright::BRIDGE_PORT;
use crate::providers::ToolDefinition;

use super::ToolError;

// ---------------------------------------------------------------------------
// Browser mode detection
// ---------------------------------------------------------------------------

/// Browser operation mode detected at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserMode {
    /// Connected to user's Chrome via CDP on the given port.
    Attached {
        /// CDP port number.
        port: u16,
    },
    /// Using Docker sidecar (headless Chromium + Playwright) on the given port.
    Standalone {
        /// Sidecar bridge port number.
        port: u16,
    },
    /// No browser available. Tool not exposed to LLM.
    None,
}

/// Port used by the Playwright sidecar bridge.
///
/// Re-exported from [`crate::executor::playwright::BRIDGE_PORT`] for
/// convenience in code that only depends on `tools::browser`.
pub const SIDECAR_PORT: u16 = BRIDGE_PORT;

/// Detect the available browser mode.
///
/// Tries CDP connection first (not yet implemented), then falls back to
/// Docker sidecar if `standalone_fallback` is enabled, otherwise returns
/// [`BrowserMode::None`].
pub async fn detect_browser(config: &crate::config::BrowserConfig) -> BrowserMode {
    // TODO: Phase 2 — try CDP connection to config.cdp_port
    // if let Ok(_tabs) = cdp_list_tabs(config.cdp_port).await {
    //     return BrowserMode::Attached { port: config.cdp_port };
    // }

    if config.standalone_fallback {
        return BrowserMode::Standalone { port: BRIDGE_PORT };
    }

    BrowserMode::None
}

/// Allowed browser actions per the tool schema.
const ALLOWED_ACTIONS: &[&str] = &[
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
];

/// Maximum length for string parameters (URL, selector, text, etc.).
const MAX_STRING_PARAM_LEN: usize = 16 * 1024;

/// Maximum timeout in milliseconds.
const MAX_TIMEOUT_MS: u64 = 120_000;

/// Default timeout in milliseconds.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

// ---------------------------------------------------------------------------
// Bridge trait
// ---------------------------------------------------------------------------

/// Optional bridge for browser automation.
///
/// When `None`, the browser tool returns a clear unavailable error.
/// Implementations must NOT use host process-command APIs
/// for user-controlled input — the bridge is expected to be a safe interface
/// (e.g. MCP client, HTTP service, or future sandboxed subprocess).
#[async_trait]
pub trait BrowserBridge: Send + Sync {
    /// Execute a validated browser action.
    ///
    /// # Errors
    ///
    /// Returns an error if the bridge cannot execute the action.
    async fn execute(&self, _action: &str, _input: &serde_json::Value) -> Result<String, String> {
        Err("browser bridge not implemented".to_owned())
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Sanitise a string parameter from input, enforcing max length.
///
/// Returns `Ok(Some(json!(value)))` if present and valid, `Ok(None)` if absent,
/// or `Err` if present but exceeds max length.
fn sanitise_string_param(
    input: &serde_json::Value,
    key: &str,
    max_len: usize,
) -> Result<Option<serde_json::Value>, ToolError> {
    let Some(s) = input.get(key).and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    if s.len() > max_len {
        return Err(ToolError::InvalidInput(format!(
            "{key} exceeds maximum length of {max_len} characters"
        )));
    }
    Ok(Some(json!(s)))
}

/// Validate browser tool input and return a sanitised structure.
///
/// # Errors
///
/// Returns [`ToolError::InvalidInput`] if the action is missing, invalid, or
/// any parameter exceeds safety limits.
pub fn validate_browser_input(input: &serde_json::Value) -> Result<serde_json::Value, ToolError> {
    let action = input
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: action".to_owned()))?;

    if !ALLOWED_ACTIONS.contains(&action) {
        return Err(ToolError::InvalidInput(format!(
            "invalid action: {action}. Allowed: {}",
            ALLOWED_ACTIONS.join(", ")
        )));
    }

    let mut sanitised = serde_json::Map::new();
    sanitised.insert("action".to_owned(), json!(action));

    for key in [
        "url",
        "selector",
        "text",
        "javascript",
        "wait_for",
        "direction",
        "tab_id",
    ] {
        if let Some(sanitised_val) = sanitise_string_param(input, key, MAX_STRING_PARAM_LEN)? {
            sanitised.insert(key.to_owned(), sanitised_val);
        }
    }

    let timeout_ms = input
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    if timeout_ms > MAX_TIMEOUT_MS {
        return Err(ToolError::InvalidInput(format!(
            "timeout_ms exceeds maximum of {MAX_TIMEOUT_MS}"
        )));
    }
    sanitised.insert("timeout_ms".to_owned(), json!(timeout_ms));

    Ok(serde_json::Value::Object(sanitised))
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Execute a browser action via the optional bridge.
///
/// When no bridge is configured, returns a clear unavailable error.
/// Enforces rate limiting before execution.
///
/// # Errors
///
/// Returns [`ToolError`] on validation failure, rate limit, or bridge unavailability.
pub async fn run_browser(
    input: &serde_json::Value,
    limiter: &RateLimiter,
    bridge: Option<&dyn BrowserBridge>,
) -> Result<String, ToolError> {
    let sanitised = validate_browser_input(input)?;

    limiter.check("browser")?;
    limiter.record();

    let bridge = bridge.ok_or_else(|| {
        ToolError::ExecutionFailed(
            "browser tool unavailable: no runtime bridge configured. \
             Configure a browser bridge (e.g. MCP or Playwright service) to enable browser automation."
                .to_owned(),
        )
    })?;

    let action = sanitised
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // SSRF / domain policy check for any action that carries a URL.
    // Currently: navigate and new_tab both accept a url parameter.
    if action == "navigate" || action == "new_tab" {
        if let Some(url_str) = sanitised.get("url").and_then(|v| v.as_str()) {
            let parsed = Url::parse(url_str).map_err(|e| {
                ToolError::InvalidInput(format!("invalid url for {action} action: {e}"))
            })?;
            ssrf_check(&parsed).await?;
        }
        // navigate requires url; new_tab may open a blank tab without one.
        if action == "navigate" && sanitised.get("url").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::InvalidInput(
                "navigate action requires url".to_owned(),
            ));
        }
    }

    debug!(action, "executing browser action via bridge");

    bridge
        .execute(action, &sanitised)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("browser bridge error: {e}")))
}

// ---------------------------------------------------------------------------
// Tool definition
// ---------------------------------------------------------------------------

/// Return the browser tool definition for inclusion in core tools.
pub fn browser_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "browser".to_owned(),
        description: "Control the browser. Can use your existing Chrome session (same cookies/logins) or a standalone instance. Navigate, click, type, screenshot, extract.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["navigate", "click", "type", "screenshot", "extract", "wait", "scroll", "evaluate", "list_tabs", "switch_tab", "new_tab", "close_tab"],
                    "description": "Browser action to perform"
                },
                "url": { "type": "string", "description": "URL for navigate action" },
                "selector": { "type": "string", "description": "CSS/XPath selector for click/type/extract" },
                "text": { "type": "string", "description": "Text for type action" },
                "javascript": { "type": "string", "description": "JS code for evaluate action" },
                "wait_for": { "type": "string", "description": "Selector or 'networkidle' for wait action" },
                "tab_id": { "type": "string", "description": "Target tab (from list_tabs). Default: active tab." },
                "timeout_ms": { "type": "integer", "default": 30000, "description": "Timeout in milliseconds" }
            },
            "required": ["action"]
        }),
    }
}
