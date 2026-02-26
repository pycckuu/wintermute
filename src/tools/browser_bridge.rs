//! Concrete [`BrowserBridge`] implementation via HTTP.
//!
//! Connects to the Playwright sidecar container's Flask bridge server
//! and translates browser actions into HTTP POST requests.

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{debug, warn};

use super::browser::BrowserBridge;

/// Default request timeout in milliseconds when the input does not specify one.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Extra buffer added to the input timeout for the HTTP request itself,
/// allowing the bridge server time to respond after the Playwright
/// operation completes.
const TIMEOUT_BUFFER_MS: u64 = 5_000;

/// HTTP connect timeout for the reqwest client.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// HTTP-based browser bridge connecting to the Playwright sidecar.
///
/// Sends validated browser actions as JSON POST requests to the bridge
/// server's `/execute` endpoint and parses the `{success, result/error}`
/// response envelope.
pub struct PlaywrightBridge {
    client: reqwest::Client,
    base_url: String,
}

impl PlaywrightBridge {
    /// Create a new bridge pointing at the sidecar's HTTP API.
    ///
    /// The `base_url` should be the root URL of the bridge server
    /// (e.g. `http://127.0.0.1:9223`).
    pub fn new(base_url: String) -> Self {
        // Build with a connect timeout; per-request timeout is set dynamically.
        // The builder can only fail on TLS backend init; fall back to default
        // client (without connect_timeout) in that unlikely scenario.
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .unwrap_or_else(|e| {
                warn!(error = %e, "failed to build HTTP client with connect timeout, using default");
                reqwest::Client::default()
            });

        Self { client, base_url }
    }
}

/// Response envelope from the bridge server.
#[derive(Debug, Deserialize)]
struct BridgeResponse {
    success: bool,
    result: Option<String>,
    error: Option<String>,
}

#[async_trait]
impl BrowserBridge for PlaywrightBridge {
    /// Execute a validated browser action via the sidecar HTTP API.
    ///
    /// Sends a POST to `{base_url}/execute` with the sanitised input JSON.
    /// The HTTP timeout is derived from `timeout_ms` in the input plus a
    /// 5-second buffer.
    async fn execute(&self, action: &str, input: &serde_json::Value) -> Result<String, String> {
        let timeout_ms = input
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_MS);

        let request_timeout_ms = timeout_ms.saturating_add(TIMEOUT_BUFFER_MS);
        let request_timeout = std::time::Duration::from_millis(request_timeout_ms);

        let url = format!("{}/execute", self.base_url);

        debug!(action, timeout_ms, "sending browser action to sidecar");

        let response = self
            .client
            .post(&url)
            .json(input)
            .timeout(request_timeout)
            .send()
            .await
            .map_err(|e| format!("bridge request failed: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            return Err(format!("bridge returned HTTP {status}"));
        }

        let body: BridgeResponse = response
            .json()
            .await
            .map_err(|e| format!("failed to parse bridge response: {e}"))?;

        if body.success {
            body.result
                .ok_or_else(|| "bridge returned success with no result".to_owned())
        } else {
            Err(body
                .error
                .unwrap_or_else(|| "bridge returned failure with no error message".to_owned()))
        }
    }
}
