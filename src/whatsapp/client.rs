//! HTTP client for the wintermute-whatsapp sidecar.
//!
//! All WhatsApp operations go through this client, which communicates
//! with the baileys-based Node.js bridge via HTTP on port 3001.

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::WhatsAppError;

/// Default port the WhatsApp bridge listens on.
pub const DEFAULT_BRIDGE_PORT: u16 = 3001;

/// HTTP connect timeout for the reqwest client.
const CONNECT_TIMEOUT_SECS: u64 = 5;

/// HTTP request timeout for normal operations.
const REQUEST_TIMEOUT_SECS: u64 = 30;

/// Number of health-check retries before giving up.
const HEALTH_CHECK_RETRIES: u32 = 5;

/// Delay between health-check attempts in milliseconds.
const HEALTH_CHECK_DELAY_MS: u64 = 2000;

/// Client for the wintermute-whatsapp HTTP bridge.
pub struct WhatsAppClient {
    client: reqwest::Client,
    base_url: String,
}

/// A WhatsApp message (inbound or outbound).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhatsAppMessage {
    /// WhatsApp JID of the conversation.
    pub jid: String,
    /// Message text content.
    pub text: String,
    /// Whether this message was sent by us.
    pub from_me: bool,
    /// ISO 8601 timestamp, if available.
    pub timestamp: Option<String>,
    /// Bridge-assigned message identifier.
    pub message_id: Option<String>,
}

/// A WhatsApp contact entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhatsAppContact {
    /// WhatsApp JID.
    pub jid: String,
    /// Display name, if known.
    pub name: Option<String>,
    /// Phone number, if known.
    pub phone: Option<String>,
}

/// Connection status from the sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhatsAppStatus {
    /// Whether the sidecar is connected to WhatsApp.
    pub connected: bool,
    /// The phone number linked, if connected.
    pub phone_number: Option<String>,
}

/// Response envelope from the bridge HTTP API.
#[derive(Deserialize)]
struct BridgeResponse<T> {
    #[allow(dead_code)]
    success: bool,
    data: Option<T>,
    error: Option<String>,
}

impl WhatsAppClient {
    /// Create a new client pointing at the given base URL.
    pub fn new(base_url: String) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(CONNECT_TIMEOUT_SECS))
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|e| {
                warn!(error = %e, "failed to build HTTP client with timeouts, using default");
                reqwest::Client::default()
            });
        Self { client, base_url }
    }

    /// Create a client connecting to `http://127.0.0.1:{port}`.
    pub fn with_port(port: u16) -> Self {
        Self::new(format!("http://127.0.0.1:{port}"))
    }

    /// Create a client using the default bridge port (3001).
    pub fn default_url() -> Self {
        Self::with_port(DEFAULT_BRIDGE_PORT)
    }

    /// Check whether the sidecar is healthy and connected to WhatsApp.
    pub async fn health_check(&self) -> Result<bool, WhatsAppError> {
        let url = format!("{}/status", self.base_url);
        match self.client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let body: BridgeResponse<WhatsAppStatus> = resp.json().await?;
                Ok(body.data.is_some_and(|s| s.connected))
            }
            Ok(_) => Ok(false),
            Err(_) => Ok(false),
        }
    }

    /// Wait for the sidecar to become healthy, retrying with a fixed delay.
    pub async fn wait_healthy(&self) -> Result<(), WhatsAppError> {
        for attempt in 0..HEALTH_CHECK_RETRIES {
            if self.health_check().await.unwrap_or(false) {
                return Ok(());
            }
            if attempt < HEALTH_CHECK_RETRIES.saturating_sub(1) {
                tokio::time::sleep(std::time::Duration::from_millis(HEALTH_CHECK_DELAY_MS)).await;
            }
        }
        Err(WhatsAppError::SidecarNotRunning)
    }

    /// Get the current connection status from the sidecar.
    pub async fn status(&self) -> Result<WhatsAppStatus, WhatsAppError> {
        let url = format!("{}/status", self.base_url);
        let resp = self.client.get(&url).send().await?;
        let body: BridgeResponse<WhatsAppStatus> = resp.json().await?;
        body.data.ok_or(WhatsAppError::SidecarNotRunning)
    }

    /// Get a QR code for WhatsApp Web linking (returned as base64 PNG).
    pub async fn get_qr(&self) -> Result<String, WhatsAppError> {
        let url = format!("{}/qr", self.base_url);
        let resp = self.client.get(&url).send().await?;
        let body: BridgeResponse<String> = resp.json().await?;
        body.data.ok_or_else(|| {
            WhatsAppError::SetupFailed(
                body.error
                    .unwrap_or_else(|| "no QR code available".to_owned()),
            )
        })
    }

    /// Send a text message to the given JID.
    pub async fn send_text(&self, jid: &str, text: &str) -> Result<(), WhatsAppError> {
        let url = format!("{}/send", self.base_url);
        let body = serde_json::json!({ "jid": jid, "text": text });
        let resp = self.client.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            warn!(%status, "WhatsApp send failed: {body_text}");
            return Err(WhatsAppError::NotConnected);
        }
        debug!(jid, "message sent via WhatsApp");
        Ok(())
    }

    /// Get recent messages from a contact by JID.
    pub async fn get_messages(
        &self,
        jid: &str,
        limit: u32,
    ) -> Result<Vec<WhatsAppMessage>, WhatsAppError> {
        let url = format!("{}/messages/{jid}?limit={limit}", self.base_url);
        let resp = self.client.get(&url).send().await?;
        let body: BridgeResponse<Vec<WhatsAppMessage>> = resp.json().await?;
        Ok(body.data.unwrap_or_default())
    }

    /// Get the contact list from the sidecar.
    pub async fn get_contacts(&self) -> Result<Vec<WhatsAppContact>, WhatsAppError> {
        let url = format!("{}/contacts", self.base_url);
        let resp = self.client.get(&url).send().await?;
        let body: BridgeResponse<Vec<WhatsAppContact>> = resp.json().await?;
        Ok(body.data.unwrap_or_default())
    }

    /// Mark messages in a conversation as read.
    pub async fn mark_read(&self, jid: &str) -> Result<(), WhatsAppError> {
        let url = format!("{}/mark-read", self.base_url);
        let body = serde_json::json!({ "jid": jid });
        self.client.post(&url).json(&body).send().await?;
        Ok(())
    }

    /// Send a typing indicator (composing) to the given JID.
    ///
    /// This is fire-and-forget: errors are silently ignored because typing
    /// indicators are cosmetic and should never block message delivery.
    pub async fn send_typing(&self, jid: &str) -> Result<(), WhatsAppError> {
        let url = format!("{}/typing", self.base_url);
        let body = serde_json::json!({ "jid": jid });
        // Fire and forget — don't fail on typing indicator issues
        let _ = self.client.post(&url).json(&body).send().await;
        Ok(())
    }

    /// Returns the base URL of the sidecar.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}
