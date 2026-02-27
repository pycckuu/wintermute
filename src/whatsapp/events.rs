//! Event listener for incoming WhatsApp messages.
//!
//! Connects to the baileys sidecar's `/events/poll` HTTP long-polling endpoint
//! and dispatches incoming messages to the router via an mpsc channel.

use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// An incoming WhatsApp event from the sidecar.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum WhatsAppEvent {
    /// A new message was received (or sent by us).
    #[serde(rename = "message")]
    Message {
        /// WhatsApp JID of the conversation.
        jid: String,
        /// Message text content.
        text: String,
        /// Whether this message was sent by us.
        from_me: bool,
        /// Bridge-assigned message identifier.
        message_id: Option<String>,
    },
    /// WhatsApp connection established.
    #[serde(rename = "connected")]
    Connected,
    /// WhatsApp connection lost.
    #[serde(rename = "disconnected")]
    Disconnected {
        /// Human-readable reason, if available.
        reason: Option<String>,
    },
}

/// Long-poll timeout for the HTTP client (seconds).
const POLL_TIMEOUT_SECS: u64 = 60;

/// Maximum reconnect backoff (milliseconds).
const MAX_BACKOFF_MS: u64 = 30_000;

/// Spawn an event listener that forwards events to the given channel.
///
/// Returns immediately. The listener runs as a background Tokio task and
/// reconnects automatically on disconnect with exponential backoff.
pub fn spawn_event_listener(
    base_url: String,
    event_tx: mpsc::Sender<WhatsAppEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let poll_url = format!("{base_url}/events/poll");
        let mut backoff_ms: u64 = 1000;

        loop {
            info!(url = %poll_url, "connecting to WhatsApp event stream");

            match poll_events(&poll_url, &event_tx).await {
                Ok(()) => {
                    info!("WhatsApp event stream closed normally");
                    break;
                }
                Err(e) => {
                    warn!(error = %e, backoff_ms, "WhatsApp event stream error, reconnecting");
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    backoff_ms = backoff_ms.saturating_mul(2).min(MAX_BACKOFF_MS);
                }
            }
        }
    })
}

/// Poll the sidecar for events in a loop. Returns `Err` on non-timeout
/// network errors so the caller can reconnect with backoff.
async fn poll_events(
    poll_url: &str,
    event_tx: &mpsc::Sender<WhatsAppEvent>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(POLL_TIMEOUT_SECS))
        .build()?;

    loop {
        match client.get(poll_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(events) = resp.json::<Vec<WhatsAppEvent>>().await {
                    for event in events {
                        debug!(?event, "received WhatsApp event");
                        if event_tx.send(event).await.is_err() {
                            // Receiver dropped — shut down cleanly.
                            return Ok(());
                        }
                    }
                }
            }
            Ok(resp) => {
                debug!(status = %resp.status(), "event poll returned non-200");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            Err(e) if e.is_timeout() => {
                // Normal: long-poll timeout expired, just retry immediately.
                continue;
            }
            Err(e) => {
                return Err(e.into());
            }
        }
    }
}
