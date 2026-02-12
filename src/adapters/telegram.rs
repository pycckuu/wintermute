//! Telegram Bot API adapter -- in-process async polling (spec 6.9, 12.1).
//!
//! Polls `getUpdates` for incoming messages, normalizes them into
//! [`InboundEvent`]s, and sends outbound messages via `sendMessage`.
//! Inline keyboards support the approval queue (spec 6.6).

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::kernel::journal::TaskJournal;
use crate::types::{EventKind, EventPayload, EventSource, InboundEvent, Principal};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Telegram adapter configuration (spec 18.1).
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    /// Bot API token (from vault at startup).
    pub bot_token: String,
    /// Telegram user ID of the system owner.
    pub owner_id: String,
    /// Long-poll timeout for `getUpdates`, in seconds.
    pub poll_timeout_seconds: u32,
}

// ---------------------------------------------------------------------------
// Adapter <-> Kernel channel messages
// ---------------------------------------------------------------------------

/// Messages from adapter to kernel (spec 6.9).
#[derive(Debug)]
pub enum AdapterToKernel {
    /// A normalized inbound event (boxed to keep enum size small).
    Event(Box<InboundEvent>),
    /// Health heartbeat (spec 14.4).
    Heartbeat,
}

/// Messages from kernel to adapter (spec 6.9).
#[derive(Debug)]
pub enum KernelToAdapter {
    /// Send a plain text message to a Telegram chat.
    SendMessage {
        /// Target chat ID.
        chat_id: String,
        /// Message text.
        text: String,
    },
    /// Send an approval request with inline Approve/Deny buttons (spec 6.6).
    SendApprovalRequest {
        /// Target chat ID.
        chat_id: String,
        /// Approval description text.
        text: String,
        /// Unique approval identifier encoded into callback data.
        approval_id: Uuid,
    },
    /// Gracefully stop the adapter.
    Shutdown,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Telegram adapter errors.
#[derive(Debug, Error)]
pub enum AdapterError {
    /// The Telegram API returned an error response.
    #[error("Telegram API error: {0}")]
    ApiError(String),
    /// The kernel channel was closed unexpectedly.
    #[error("channel closed")]
    ChannelClosed,
    /// HTTP transport error.
    #[error("HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),
    /// JSON deserialization failed.
    #[error("JSON parse error: {0}")]
    ParseError(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------
// Telegram API types (minimal subset)
// ---------------------------------------------------------------------------

/// Generic Telegram Bot API response wrapper.
#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

/// Telegram `Update` object.
#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
    callback_query: Option<TelegramCallbackQuery>,
}

/// Telegram `Message` object (subset of fields we use).
#[derive(Debug, Deserialize)]
struct TelegramMessage {
    message_id: i64,
    from: Option<TelegramUser>,
    chat: TelegramChat,
    text: Option<String>,
}

/// Telegram `User` object.
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // fields required for deserialization from Telegram API
struct TelegramUser {
    id: i64,
    first_name: String,
}

/// Telegram `Chat` object.
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // fields required for deserialization from Telegram API
struct TelegramChat {
    id: i64,
    #[serde(rename = "type")]
    chat_type: String,
}

/// Telegram `CallbackQuery` object.
#[derive(Debug, Deserialize)]
struct TelegramCallbackQuery {
    id: String,
    from: TelegramUser,
    data: Option<String>,
    message: Option<TelegramMessage>,
}

/// Inline keyboard markup for approval buttons (spec 6.6).
#[derive(Debug, Serialize)]
struct InlineKeyboardMarkup {
    inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

/// A single inline keyboard button.
#[derive(Debug, Serialize)]
struct InlineKeyboardButton {
    text: String,
    callback_data: String,
}

// ---------------------------------------------------------------------------
// Adapter implementation
// ---------------------------------------------------------------------------

/// Base URL for the Telegram Bot API.
const TELEGRAM_API_BASE: &str = "https://api.telegram.org";

/// Initial backoff on poll failure, in milliseconds.
const INITIAL_BACKOFF_MS: u64 = 1_000;

/// Maximum backoff on poll failure, in milliseconds.
const MAX_BACKOFF_MS: u64 = 30_000;

/// Heartbeat interval in seconds (spec 14.4).
const HEARTBEAT_INTERVAL_SECS: u64 = 60;

/// Extra seconds added to the HTTP timeout beyond the long-poll timeout,
/// so the TCP socket stays open while Telegram holds the request.
const POLL_TIMEOUT_MARGIN_SECS: u64 = 10;

/// Telegram Bot API adapter (spec 6.9).
///
/// Runs as a long-lived tokio task. Polls `getUpdates` in a loop,
/// normalizes updates into [`InboundEvent`]s, and forwards them to the
/// kernel via an mpsc channel. Outbound messages are received from the
/// kernel on a separate channel.
pub struct TelegramAdapter {
    config: TelegramConfig,
    client: reqwest::Client,
    /// Optional task journal for persisting adapter state (spec 8.2).
    journal: Option<Arc<TaskJournal>>,
}

impl TelegramAdapter {
    /// Create a new Telegram adapter (spec 6.9).
    pub fn new(config: TelegramConfig, journal: Option<Arc<TaskJournal>>) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            journal,
        }
    }

    /// Run the adapter as a long-lived tokio task (spec 6.9).
    ///
    /// Spawns three concurrent activities:
    /// 1. **Inbound poll loop** -- long-polls `getUpdates` and sends events to the kernel.
    /// 2. **Outbound handler** -- receives messages from the kernel and calls `sendMessage`.
    /// 3. **Heartbeat** -- sends a heartbeat to the kernel every 60 seconds (spec 14.4).
    ///
    /// Returns when the kernel channel is closed or a `Shutdown` command is received.
    pub async fn run(
        self,
        to_kernel: mpsc::Sender<AdapterToKernel>,
        mut from_kernel: mpsc::Receiver<KernelToAdapter>,
    ) -> Result<(), AdapterError> {
        info!("Telegram adapter starting");

        // Spawn outbound handler
        let client_out = self.client.clone();
        let token_out = self.config.bot_token.clone();
        let outbound_handle = tokio::spawn(async move {
            Self::outbound_loop(client_out, &token_out, &mut from_kernel).await;
        });

        // Spawn heartbeat
        let heartbeat_tx = to_kernel.clone();
        let heartbeat_handle = tokio::spawn(async move {
            Self::heartbeat_loop(heartbeat_tx).await;
        });

        // Run the inbound poll loop on this task
        let poll_result = self.poll_loop(&to_kernel).await;

        // Clean up spawned tasks
        outbound_handle.abort();
        heartbeat_handle.abort();

        info!("Telegram adapter stopped");
        poll_result
    }

    // ------------------------------------------------------------------
    // Inbound polling
    // ------------------------------------------------------------------

    /// Main polling loop with exponential backoff on errors.
    async fn poll_loop(
        &self,
        to_kernel: &mpsc::Sender<AdapterToKernel>,
    ) -> Result<(), AdapterError> {
        // Load persisted offset from journal if available (spec 8.2).
        let mut offset: Option<i64> = self.load_persisted_offset();
        if let Some(off) = offset {
            info!(offset = off, "restored Telegram offset from journal");
        }

        let mut backoff_ms: u64 = INITIAL_BACKOFF_MS;

        loop {
            match self.poll_updates(offset).await {
                Ok(updates) => {
                    backoff_ms = INITIAL_BACKOFF_MS;

                    for update in updates {
                        // Advance offset so we don't re-process this update.
                        offset = Some(update.update_id.saturating_add(1));

                        if let Some(event) = self.normalize_update(&update) {
                            debug!(event_id = %event.event_id, "normalized Telegram update");
                            if to_kernel
                                .send(AdapterToKernel::Event(Box::new(event)))
                                .await
                                .is_err()
                            {
                                info!("kernel channel closed, shutting down poll loop");
                                return Ok(());
                            }
                        }
                    }

                    // Persist offset after each successful batch (best-effort).
                    self.persist_offset(offset);
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        backoff_ms,
                        "Telegram poll error, backing off"
                    );
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    backoff_ms = backoff_ms.saturating_mul(2).min(MAX_BACKOFF_MS);
                }
            }
        }
    }

    /// Load the last persisted Telegram offset from the journal.
    fn load_persisted_offset(&self) -> Option<i64> {
        let journal = self.journal.as_ref()?;
        match journal.load_adapter_state("telegram") {
            Ok(Some(json_str)) => {
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(&json_str);
                match parsed {
                    Ok(val) => val.get("last_offset").and_then(|v| v.as_i64()),
                    Err(e) => {
                        warn!(error = %e, "failed to parse persisted Telegram offset");
                        None
                    }
                }
            }
            Ok(None) => None,
            Err(e) => {
                warn!(error = %e, "failed to load Telegram adapter state from journal");
                None
            }
        }
    }

    /// Persist the current Telegram offset to the journal (best-effort).
    fn persist_offset(&self, offset: Option<i64>) {
        if let (Some(journal), Some(off)) = (self.journal.as_ref(), offset) {
            let json = serde_json::json!({"last_offset": off}).to_string();
            if let Err(e) = journal.save_adapter_state("telegram", &json) {
                warn!(error = %e, "failed to persist Telegram offset (best-effort)");
            }
        }
    }

    /// Call `getUpdates` on the Telegram Bot API (spec 6.9).
    async fn poll_updates(&self, offset: Option<i64>) -> Result<Vec<TelegramUpdate>, AdapterError> {
        let url = format!(
            "{}/bot{}/getUpdates",
            TELEGRAM_API_BASE, self.config.bot_token
        );

        let mut params = serde_json::json!({
            "timeout": self.config.poll_timeout_seconds,
        });
        if let Some(off) = offset {
            params["offset"] = serde_json::Value::from(off);
        }

        let http_timeout_secs =
            u64::from(self.config.poll_timeout_seconds).saturating_add(POLL_TIMEOUT_MARGIN_SECS);

        let resp = self
            .client
            .post(&url)
            .json(&params)
            .timeout(Duration::from_secs(http_timeout_secs))
            .send()
            .await?;

        let response: TelegramResponse<Vec<TelegramUpdate>> = resp.json().await?;

        if !response.ok {
            return Err(AdapterError::ApiError(
                response
                    .description
                    .unwrap_or_else(|| "unknown error".to_string()),
            ));
        }

        Ok(response.result.unwrap_or_default())
    }

    // ------------------------------------------------------------------
    // Principal resolution
    // ------------------------------------------------------------------

    /// Map a Telegram user ID to a [`Principal`] (spec 6.9, 4.1).
    ///
    /// If the user ID matches `config.owner_id`, the principal is `Owner`.
    /// Otherwise it is `TelegramPeer(<user_id>)`.
    fn resolve_principal(&self, user_id: i64) -> Principal {
        let user_id_str = user_id.to_string();
        if user_id_str == self.config.owner_id {
            Principal::Owner
        } else {
            Principal::TelegramPeer(user_id_str)
        }
    }

    // ------------------------------------------------------------------
    // Event normalization
    // ------------------------------------------------------------------

    /// Normalize a Telegram `Update` into an [`InboundEvent`] (spec 10.1).
    ///
    /// Returns `None` for update types we don't handle (e.g. edited messages,
    /// channel posts, or messages without a `from` field).
    fn normalize_update(&self, update: &TelegramUpdate) -> Option<InboundEvent> {
        // Handle regular messages.
        if let Some(msg) = &update.message {
            return self.normalize_message(msg);
        }

        // Handle callback queries (approval button presses, spec 6.6).
        if let Some(cb) = &update.callback_query {
            return Some(self.normalize_callback(cb));
        }

        None
    }

    /// Normalize a regular `TelegramMessage` into an [`InboundEvent`].
    fn normalize_message(&self, msg: &TelegramMessage) -> Option<InboundEvent> {
        let user = msg.from.as_ref()?;
        let principal = self.resolve_principal(user.id);

        Some(InboundEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            source: EventSource {
                adapter: "telegram".to_string(),
                principal,
            },
            kind: EventKind::Message,
            payload: EventPayload {
                text: msg.text.clone(),
                attachments: vec![],
                reply_to: None,
                metadata: serde_json::json!({
                    "chat_id": msg.chat.id.to_string(),
                    "message_id": msg.message_id.to_string(),
                }),
            },
        })
    }

    /// Normalize a `TelegramCallbackQuery` into an [`InboundEvent`].
    fn normalize_callback(&self, cb: &TelegramCallbackQuery) -> InboundEvent {
        let principal = self.resolve_principal(cb.from.id);
        let chat_id = cb
            .message
            .as_ref()
            .map(|m| m.chat.id.to_string())
            .unwrap_or_default();

        InboundEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            source: EventSource {
                adapter: "telegram".to_string(),
                principal,
            },
            kind: EventKind::Callback,
            payload: EventPayload {
                text: cb.data.clone(),
                attachments: vec![],
                reply_to: None,
                metadata: serde_json::json!({
                    "chat_id": chat_id,
                    "callback_query_id": cb.id,
                }),
            },
        }
    }

    // ------------------------------------------------------------------
    // Outbound message handling
    // ------------------------------------------------------------------

    /// Process outbound commands from the kernel until the channel closes
    /// or a `Shutdown` command is received.
    async fn outbound_loop(
        client: reqwest::Client,
        bot_token: &str,
        from_kernel: &mut mpsc::Receiver<KernelToAdapter>,
    ) {
        while let Some(msg) = from_kernel.recv().await {
            match msg {
                KernelToAdapter::SendMessage { chat_id, text } => {
                    if let Err(e) =
                        Self::send_message(&client, bot_token, &chat_id, &text, None).await
                    {
                        error!(error = %e, chat_id, "failed to send Telegram message");
                    }
                }
                KernelToAdapter::SendApprovalRequest {
                    chat_id,
                    text,
                    approval_id,
                } => {
                    let keyboard = InlineKeyboardMarkup {
                        inline_keyboard: vec![vec![
                            InlineKeyboardButton {
                                text: "Approve".to_string(),
                                callback_data: format!("approve:{approval_id}"),
                            },
                            InlineKeyboardButton {
                                text: "Deny".to_string(),
                                callback_data: format!("deny:{approval_id}"),
                            },
                        ]],
                    };
                    if let Err(e) =
                        Self::send_message(&client, bot_token, &chat_id, &text, Some(keyboard))
                            .await
                    {
                        error!(error = %e, chat_id, "failed to send approval request");
                    }
                }
                KernelToAdapter::Shutdown => {
                    info!("Telegram adapter outbound loop shutting down");
                    break;
                }
            }
        }
    }

    /// Send a message via the Telegram Bot API `sendMessage` endpoint.
    async fn send_message(
        client: &reqwest::Client,
        bot_token: &str,
        chat_id: &str,
        text: &str,
        reply_markup: Option<InlineKeyboardMarkup>,
    ) -> Result<(), AdapterError> {
        let url = format!("{}/bot{}/sendMessage", TELEGRAM_API_BASE, bot_token);

        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
        });

        if let Some(markup) = reply_markup {
            body["reply_markup"] =
                serde_json::to_value(markup).map_err(|e| AdapterError::ApiError(e.to_string()))?;
        }

        let resp = client.post(&url).json(&body).send().await?;
        let response: TelegramResponse<serde_json::Value> = resp.json().await?;

        if !response.ok {
            return Err(AdapterError::ApiError(
                response
                    .description
                    .unwrap_or_else(|| "sendMessage failed".to_string()),
            ));
        }

        debug!(chat_id, "sent Telegram message");
        Ok(())
    }

    // ------------------------------------------------------------------
    // Heartbeat
    // ------------------------------------------------------------------

    /// Send periodic heartbeats to the kernel (spec 14.4).
    async fn heartbeat_loop(tx: mpsc::Sender<AdapterToKernel>) {
        let mut interval = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
        loop {
            interval.tick().await;
            if tx.send(AdapterToKernel::Heartbeat).await.is_err() {
                debug!("kernel channel closed, stopping heartbeat");
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> TelegramConfig {
        TelegramConfig {
            bot_token: "test-token".to_string(),
            owner_id: "415494855".to_string(),
            poll_timeout_seconds: 30,
        }
    }

    fn make_adapter() -> TelegramAdapter {
        TelegramAdapter::new(test_config(), None)
    }

    // -- resolve_principal --

    #[test]
    fn resolve_principal_owner() {
        let adapter = make_adapter();
        assert_eq!(adapter.resolve_principal(415_494_855), Principal::Owner);
    }

    #[test]
    fn resolve_principal_peer() {
        let adapter = make_adapter();
        let principal = adapter.resolve_principal(123_456);
        assert_eq!(principal, Principal::TelegramPeer("123456".to_string()));
    }

    // -- normalize_update: regular messages --

    #[test]
    fn normalize_text_message_from_owner() {
        let adapter = make_adapter();
        let update = TelegramUpdate {
            update_id: 1,
            message: Some(TelegramMessage {
                message_id: 42,
                from: Some(TelegramUser {
                    id: 415_494_855,
                    first_name: "Igor".to_string(),
                }),
                chat: TelegramChat {
                    id: 415_494_855,
                    chat_type: "private".to_string(),
                },
                text: Some("check my email".to_string()),
            }),
            callback_query: None,
        };

        let event = adapter
            .normalize_update(&update)
            .expect("should produce event");
        assert_eq!(event.source.adapter, "telegram");
        assert_eq!(event.source.principal, Principal::Owner);
        assert!(matches!(event.kind, EventKind::Message));
        assert_eq!(event.payload.text.as_deref(), Some("check my email"));
        // metadata should carry chat_id
        assert_eq!(event.payload.metadata["chat_id"], "415494855");
    }

    #[test]
    fn normalize_text_message_from_peer() {
        let adapter = make_adapter();
        let update = TelegramUpdate {
            update_id: 10,
            message: Some(TelegramMessage {
                message_id: 99,
                from: Some(TelegramUser {
                    id: 777,
                    first_name: "Alice".to_string(),
                }),
                chat: TelegramChat {
                    id: 777,
                    chat_type: "private".to_string(),
                },
                text: Some("hello".to_string()),
            }),
            callback_query: None,
        };

        let event = adapter
            .normalize_update(&update)
            .expect("should produce event");
        assert_eq!(
            event.source.principal,
            Principal::TelegramPeer("777".to_string())
        );
        assert_eq!(event.payload.text.as_deref(), Some("hello"));
    }

    #[test]
    fn normalize_message_no_from_skipped() {
        let adapter = make_adapter();
        let update = TelegramUpdate {
            update_id: 4,
            message: Some(TelegramMessage {
                message_id: 1,
                from: None,
                chat: TelegramChat {
                    id: 1,
                    chat_type: "private".to_string(),
                },
                text: Some("hi".to_string()),
            }),
            callback_query: None,
        };
        assert!(
            adapter.normalize_update(&update).is_none(),
            "messages without a from field must be skipped"
        );
    }

    #[test]
    fn normalize_message_no_text() {
        let adapter = make_adapter();
        let update = TelegramUpdate {
            update_id: 5,
            message: Some(TelegramMessage {
                message_id: 2,
                from: Some(TelegramUser {
                    id: 415_494_855,
                    first_name: "Igor".to_string(),
                }),
                chat: TelegramChat {
                    id: 415_494_855,
                    chat_type: "private".to_string(),
                },
                text: None,
            }),
            callback_query: None,
        };

        let event = adapter
            .normalize_update(&update)
            .expect("should produce event even without text");
        assert!(event.payload.text.is_none());
    }

    // -- normalize_update: callback queries --

    #[test]
    fn normalize_callback_query() {
        let adapter = make_adapter();
        let update = TelegramUpdate {
            update_id: 2,
            message: None,
            callback_query: Some(TelegramCallbackQuery {
                id: "cb123".to_string(),
                from: TelegramUser {
                    id: 415_494_855,
                    first_name: "Igor".to_string(),
                },
                data: Some("approve:some-uuid".to_string()),
                message: Some(TelegramMessage {
                    message_id: 10,
                    from: None,
                    chat: TelegramChat {
                        id: 415_494_855,
                        chat_type: "private".to_string(),
                    },
                    text: None,
                }),
            }),
        };

        let event = adapter
            .normalize_update(&update)
            .expect("should produce event");
        assert!(matches!(event.kind, EventKind::Callback));
        assert_eq!(event.source.principal, Principal::Owner);
        assert_eq!(event.payload.text.as_deref(), Some("approve:some-uuid"));
        assert_eq!(event.payload.metadata["callback_query_id"], "cb123");
    }

    #[test]
    fn normalize_callback_query_no_message() {
        let adapter = make_adapter();
        let update = TelegramUpdate {
            update_id: 6,
            message: None,
            callback_query: Some(TelegramCallbackQuery {
                id: "cb456".to_string(),
                from: TelegramUser {
                    id: 999,
                    first_name: "Bob".to_string(),
                },
                data: Some("deny:xyz".to_string()),
                message: None,
            }),
        };

        let event = adapter
            .normalize_update(&update)
            .expect("callback without message should still produce event");
        assert!(matches!(event.kind, EventKind::Callback));
        assert_eq!(
            event.source.principal,
            Principal::TelegramPeer("999".to_string())
        );
        // chat_id falls back to empty string when message is absent
        assert_eq!(event.payload.metadata["chat_id"], "");
    }

    // -- empty update --

    #[test]
    fn normalize_empty_update_returns_none() {
        let adapter = make_adapter();
        let update = TelegramUpdate {
            update_id: 3,
            message: None,
            callback_query: None,
        };
        assert!(adapter.normalize_update(&update).is_none());
    }

    // -- serialization --

    #[test]
    fn inline_keyboard_serialization() {
        let keyboard = InlineKeyboardMarkup {
            inline_keyboard: vec![vec![
                InlineKeyboardButton {
                    text: "Approve".to_string(),
                    callback_data: "approve:123".to_string(),
                },
                InlineKeyboardButton {
                    text: "Deny".to_string(),
                    callback_data: "deny:123".to_string(),
                },
            ]],
        };
        let json =
            serde_json::to_string(&keyboard).expect("inline keyboard should serialize to JSON");
        assert!(json.contains("Approve"));
        assert!(json.contains("approve:123"));
        assert!(json.contains("Deny"));
        assert!(json.contains("deny:123"));
    }

    // -- config --

    #[test]
    fn config_fields() {
        let config = TelegramConfig {
            bot_token: "tok".to_string(),
            owner_id: "123".to_string(),
            poll_timeout_seconds: 30,
        };
        assert_eq!(config.poll_timeout_seconds, 30);
        assert_eq!(config.owner_id, "123");
    }

    // -- channel message variants --

    #[test]
    fn adapter_to_kernel_variants() {
        // Ensure the enum variants are constructible (compile-time check).
        let _event = AdapterToKernel::Event(Box::new(InboundEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            source: EventSource {
                adapter: "telegram".to_string(),
                principal: Principal::Owner,
            },
            kind: EventKind::Message,
            payload: EventPayload {
                text: Some("hi".to_string()),
                attachments: vec![],
                reply_to: None,
                metadata: serde_json::json!({}),
            },
        }));
        let _hb = AdapterToKernel::Heartbeat;
    }

    #[test]
    fn kernel_to_adapter_variants() {
        let _send = KernelToAdapter::SendMessage {
            chat_id: "1".to_string(),
            text: "hello".to_string(),
        };
        let _approval = KernelToAdapter::SendApprovalRequest {
            chat_id: "1".to_string(),
            text: "approve?".to_string(),
            approval_id: Uuid::new_v4(),
        };
        let _shutdown = KernelToAdapter::Shutdown;
    }

    // -- journal persistence (Task 5) --

    use crate::kernel::journal::TaskJournal;

    #[test]
    fn persist_offset_roundtrip() {
        let journal = Arc::new(TaskJournal::open_in_memory().expect("in-memory journal"));
        let adapter = TelegramAdapter::new(test_config(), Some(Arc::clone(&journal)));

        // Initially no persisted offset.
        assert!(adapter.load_persisted_offset().is_none());

        // Persist an offset and read it back.
        adapter.persist_offset(Some(42));
        assert_eq!(adapter.load_persisted_offset(), Some(42));

        // Update to a higher offset.
        adapter.persist_offset(Some(100));
        assert_eq!(adapter.load_persisted_offset(), Some(100));
    }

    #[test]
    fn load_persisted_offset_on_new_adapter() {
        let journal = Arc::new(TaskJournal::open_in_memory().expect("in-memory journal"));

        // First adapter persists an offset.
        let adapter1 = TelegramAdapter::new(test_config(), Some(Arc::clone(&journal)));
        adapter1.persist_offset(Some(55));

        // Second adapter (simulating restart) loads the same offset.
        let adapter2 = TelegramAdapter::new(test_config(), Some(Arc::clone(&journal)));
        assert_eq!(adapter2.load_persisted_offset(), Some(55));
    }

    #[test]
    fn none_journal_no_persistence() {
        let adapter = TelegramAdapter::new(test_config(), None);

        // No journal — persist is a no-op, load returns None.
        adapter.persist_offset(Some(10));
        assert!(adapter.load_persisted_offset().is_none());
    }

    #[test]
    fn persist_none_offset_is_noop() {
        let journal = Arc::new(TaskJournal::open_in_memory().expect("in-memory journal"));
        let adapter = TelegramAdapter::new(test_config(), Some(Arc::clone(&journal)));

        // Persist a real offset first.
        adapter.persist_offset(Some(77));
        assert_eq!(adapter.load_persisted_offset(), Some(77));

        // Persisting None offset should not overwrite the existing value.
        adapter.persist_offset(None);
        assert_eq!(adapter.load_persisted_offset(), Some(77));
    }
}
