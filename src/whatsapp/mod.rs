//! WhatsApp adapter: HTTP bridge client, event listener, setup flow, and message router.
//!
//! Communicates with a baileys-based Docker sidecar (`wintermute-whatsapp`) via
//! HTTP on port 3001 and long-polling for real-time incoming messages.

pub mod client;
pub mod events;
pub mod router;
pub mod setup;

/// Errors from the WhatsApp adapter.
#[derive(Debug, thiserror::Error)]
pub enum WhatsAppError {
    /// HTTP request to the sidecar failed.
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// The sidecar container is not running or not reachable.
    #[error("sidecar not running")]
    SidecarNotRunning,

    /// The sidecar is running but WhatsApp is not connected (needs QR scan).
    #[error("not connected to WhatsApp")]
    NotConnected,

    /// The specified contact could not be resolved.
    #[error("contact not found: {0}")]
    ContactNotFound(String),

    /// Outbound message was rate-limited.
    #[error("rate limited: {0}")]
    RateLimited(String),

    /// Container setup or lifecycle operation failed.
    #[error("setup failed: {0}")]
    SetupFailed(String),
}
