//! Credential gate — intercepts credential messages before the pipeline
//! (feature-credential-acquisition, spec 8.5, Invariant B).
//!
//! When `admin.prompt_credential` asks the owner for a token, the gate
//! registers a pending prompt. If the next owner message looks like a
//! credential (known prefix or heuristic), the gate stores it directly
//! in the vault and requests message deletion — the LLM never sees it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::kernel::journal::TaskJournal;
use crate::kernel::vault::{SecretStore, SecretValue};
use crate::types::{InboundEvent, Principal};

/// A pending credential prompt registered after `admin.prompt_credential`
/// (feature-credential-acquisition, spec 8.5).
#[derive(Debug, Clone)]
pub struct PendingCredentialPrompt {
    /// Service name (e.g., "notion").
    pub service: String,
    /// Vault reference key (e.g., "vault:notion_notion_token").
    pub vault_key: String,
    /// Expected token prefix (e.g., "ntn_") for fast classification.
    pub expected_prefix: Option<String>,
    /// When the prompt was registered.
    pub prompted_at: Instant,
    /// How long the prompt stays active.
    pub ttl: Duration,
}

/// Result of credential gate interception (feature-credential-acquisition, spec 8.5).
#[derive(Debug)]
pub enum InterceptResult {
    /// Credential was intercepted, stored in vault, message should be deleted.
    Intercepted {
        service: String,
        vault_key: String,
        chat_id: String,
        message_id: String,
    },
    /// Owner cancelled the credential prompt.
    Cancelled { service: String, chat_id: String },
    /// Not a credential message — proceed with normal pipeline.
    NotIntercepted,
}

/// Classification of a message in credential-pending context.
#[derive(Debug, PartialEq, Eq)]
enum Classification {
    /// Message looks like a credential token.
    Credential,
    /// Owner wants to cancel the pending prompt.
    Cancel,
    /// Normal message — not a credential.
    NormalMessage,
}

/// Credential gate that intercepts token pastes before the pipeline
/// (feature-credential-acquisition, spec 8.5, Invariant B).
pub struct CredentialGate {
    /// Pending prompts keyed by serialized principal.
    pending: HashMap<String, PendingCredentialPrompt>,
    /// Vault for storing intercepted credentials.
    vault: Arc<dyn SecretStore>,
    /// Journal for persistence across restarts.
    journal: Option<Arc<TaskJournal>>,
}

impl CredentialGate {
    /// Create a new credential gate, loading any persisted pending prompts
    /// from the journal (feature-credential-acquisition, spec 8.5).
    pub fn new(vault: Arc<dyn SecretStore>, journal: Option<Arc<TaskJournal>>) -> Self {
        let mut pending = HashMap::new();

        if let Some(ref j) = journal {
            match j.load_all_pending_credential_prompts() {
                Ok(prompts) => {
                    for (principal, service, vault_key, expected_prefix) in prompts {
                        debug!(principal = %principal, service = %service, "restored pending credential prompt");
                        pending.insert(
                            principal,
                            PendingCredentialPrompt {
                                service,
                                vault_key,
                                expected_prefix,
                                prompted_at: Instant::now(),
                                ttl: Duration::from_secs(600),
                            },
                        );
                    }
                }
                Err(e) => {
                    warn!(error = %e, "failed to load pending credential prompts from journal");
                }
            }
        }

        Self {
            pending,
            vault,
            journal,
        }
    }

    /// Register a pending credential prompt for a principal
    /// (feature-credential-acquisition, spec 8.5).
    pub fn register_pending(&mut self, principal: &Principal, prompt: PendingCredentialPrompt) {
        let key = serde_json::to_string(principal).unwrap_or_default();
        info!(
            principal = %key,
            service = %prompt.service,
            "registered pending credential prompt"
        );

        // Persist to journal for crash recovery.
        if let Some(ref j) = self.journal {
            if let Err(e) = j.save_pending_credential_prompt(
                &key,
                &prompt.service,
                &prompt.vault_key,
                prompt.expected_prefix.as_deref(),
            ) {
                warn!(error = %e, "failed to persist pending credential prompt");
            }
        }

        self.pending.insert(key, prompt);
    }

    /// Try to intercept an inbound event as a credential paste
    /// (feature-credential-acquisition, spec 8.5, Invariant B).
    ///
    /// Returns `Intercepted` if a credential was stored, `Cancelled` if the
    /// owner chose to cancel, or `NotIntercepted` for normal messages.
    pub async fn intercept(&mut self, event: &InboundEvent) -> InterceptResult {
        let principal_key = serde_json::to_string(&event.source.principal).unwrap_or_default();

        // Only intercept if there's a pending prompt for this principal.
        let prompt = match self.pending.get(&principal_key) {
            Some(p) => p,
            None => return InterceptResult::NotIntercepted,
        };

        // Check TTL expiry.
        if prompt.prompted_at.elapsed() > prompt.ttl {
            info!(
                principal = %principal_key,
                service = %prompt.service,
                "pending credential prompt expired"
            );
            self.remove_pending(&principal_key);
            return InterceptResult::NotIntercepted;
        }

        // Get message text.
        let text = match event.payload.text.as_deref() {
            Some(t) => t.trim(),
            None => return InterceptResult::NotIntercepted,
        };

        // Extract chat_id and message_id from event metadata.
        let chat_id = event
            .payload
            .metadata
            .get("chat_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        let message_id = event
            .payload
            .metadata
            .get("message_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();

        match classify(text, prompt) {
            Classification::Credential => {
                let service = prompt.service.clone();
                let vault_key = prompt.vault_key.clone();

                // Store credential directly in vault — never enters pipeline.
                if let Err(e) = self
                    .vault
                    .store_secret(&vault_key, SecretValue::new(text))
                    .await
                {
                    warn!(error = %e, service = %service, "failed to store credential in vault");
                    self.remove_pending(&principal_key);
                    return InterceptResult::NotIntercepted;
                }

                info!(service = %service, "credential stored via gate (Invariant B preserved)");
                self.remove_pending(&principal_key);

                // Persist message deletion for retry on crash.
                if !chat_id.is_empty() && !message_id.is_empty() {
                    if let Some(ref j) = self.journal {
                        let _ = j.save_pending_deletion(&chat_id, &message_id);
                    }
                }

                InterceptResult::Intercepted {
                    service,
                    vault_key,
                    chat_id,
                    message_id,
                }
            }
            Classification::Cancel => {
                let service = prompt.service.clone();
                self.remove_pending(&principal_key);
                InterceptResult::Cancelled { service, chat_id }
            }
            Classification::NormalMessage => InterceptResult::NotIntercepted,
        }
    }

    /// Remove a pending prompt by principal key and clean up journal.
    fn remove_pending(&mut self, principal_key: &str) {
        self.pending.remove(principal_key);
        if let Some(ref j) = self.journal {
            let _ = j.delete_pending_credential_prompt(principal_key);
        }
    }
}

/// Classify a message in credential-pending context
/// (feature-credential-acquisition, spec 8.5).
fn classify(text: &str, prompt: &PendingCredentialPrompt) -> Classification {
    let lower = text.to_lowercase();

    // Cancel keywords.
    if matches!(lower.as_str(), "cancel" | "nevermind" | "skip" | "abort") {
        return Classification::Cancel;
    }

    // Known prefix match.
    if let Some(ref prefix) = prompt.expected_prefix {
        if text.starts_with(prefix.as_str()) {
            return Classification::Credential;
        }
    }

    // Heuristic: looks like a token.
    if looks_like_token(text) {
        return Classification::Credential;
    }

    Classification::NormalMessage
}

/// Heuristic check: does this text look like an API token?
/// (feature-credential-acquisition, spec 8.5).
///
/// Criteria: length 15-500, no spaces or newlines, >90% token characters
/// (alphanumeric, `-`, `_`, `.`, `+`, `/`, `=`).
fn looks_like_token(text: &str) -> bool {
    let len = text.len();
    if !(15..=500).contains(&len) {
        return false;
    }

    // No whitespace allowed in tokens.
    if text.contains(char::is_whitespace) {
        return false;
    }

    let token_chars = text
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '='))
        .count();

    // >90% token characters.
    token_chars
        .checked_mul(10)
        .is_some_and(|scaled| scaled >= len.saturating_mul(9))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::vault::InMemoryVault;
    use crate::types::{EventKind, EventPayload, EventSource};
    use chrono::Utc;
    use uuid::Uuid;

    fn make_prompt(prefix: Option<&str>) -> PendingCredentialPrompt {
        PendingCredentialPrompt {
            service: "notion".to_string(),
            vault_key: "vault:notion_notion_token".to_string(),
            expected_prefix: prefix.map(|s| s.to_string()),
            prompted_at: Instant::now(),
            ttl: Duration::from_secs(600),
        }
    }

    fn make_event(text: &str, principal: Principal) -> InboundEvent {
        InboundEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            source: EventSource {
                adapter: "telegram".to_string(),
                principal,
            },
            kind: EventKind::Message,
            payload: EventPayload {
                text: Some(text.to_string()),
                attachments: vec![],
                reply_to: None,
                metadata: serde_json::json!({
                    "chat_id": "12345",
                    "message_id": "42",
                }),
            },
        }
    }

    // -- classify --

    #[test]
    fn classify_known_prefix() {
        let prompt = make_prompt(Some("ntn_"));
        assert_eq!(
            classify("ntn_265011509509ABCdef", &prompt),
            Classification::Credential
        );
    }

    #[test]
    fn classify_cancel() {
        let prompt = make_prompt(Some("ntn_"));
        assert_eq!(classify("cancel", &prompt), Classification::Cancel);
        assert_eq!(classify("nevermind", &prompt), Classification::Cancel);
        assert_eq!(classify("skip", &prompt), Classification::Cancel);
        assert_eq!(classify("abort", &prompt), Classification::Cancel);
    }

    #[test]
    fn classify_normal_message() {
        let prompt = make_prompt(Some("ntn_"));
        assert_eq!(
            classify("What's for lunch?", &prompt),
            Classification::NormalMessage
        );
    }

    #[test]
    fn classify_heuristic_no_prefix() {
        let prompt = make_prompt(None);
        // Long alphanumeric string should trigger heuristic.
        assert_eq!(
            classify("sk-abc123def456ghi789jkl012mno", &prompt),
            Classification::Credential
        );
    }

    #[test]
    fn classify_short_text_not_credential() {
        let prompt = make_prompt(None);
        assert_eq!(classify("hello", &prompt), Classification::NormalMessage);
    }

    // -- looks_like_token --

    #[test]
    fn looks_like_token_valid() {
        assert!(looks_like_token("ghp_ABCDEFghijklmnopqrstuvwxyz123456"));
        assert!(looks_like_token("ntn_265011509509ABCdefGHIjkl"));
        assert!(looks_like_token("xoxb-123-456-abcdefghij"));
    }

    #[test]
    fn looks_like_token_too_short() {
        assert!(!looks_like_token("abc123"));
    }

    #[test]
    fn looks_like_token_has_spaces() {
        assert!(!looks_like_token("this is a normal sentence with spaces"));
    }

    #[test]
    fn looks_like_token_edges() {
        // Exactly 15 chars, all alphanumeric — should pass.
        assert!(looks_like_token("abcdefghij12345"));
        // 14 chars — too short.
        assert!(!looks_like_token("abcdefghij1234"));
    }

    // -- intercept --

    #[tokio::test]
    async fn intercept_stores_credential() {
        let vault: Arc<dyn SecretStore> = Arc::new(InMemoryVault::new());
        let mut gate = CredentialGate::new(Arc::clone(&vault), None);

        gate.register_pending(&Principal::Owner, make_prompt(Some("ntn_")));

        let event = make_event("ntn_265011509509ABCdefGHIjkl", Principal::Owner);
        let result = gate.intercept(&event).await;

        match result {
            InterceptResult::Intercepted {
                service, vault_key, ..
            } => {
                assert_eq!(service, "notion");
                assert_eq!(vault_key, "vault:notion_notion_token");
            }
            other => panic!("expected Intercepted, got {other:?}"),
        }

        // Verify credential is in vault.
        let secret = vault
            .get_secret("vault:notion_notion_token")
            .await
            .expect("secret should be stored");
        assert_eq!(secret.expose(), "ntn_265011509509ABCdefGHIjkl");
    }

    #[tokio::test]
    async fn intercept_cancel() {
        let vault: Arc<dyn SecretStore> = Arc::new(InMemoryVault::new());
        let mut gate = CredentialGate::new(Arc::clone(&vault), None);

        gate.register_pending(&Principal::Owner, make_prompt(Some("ntn_")));

        let event = make_event("cancel", Principal::Owner);
        let result = gate.intercept(&event).await;

        match result {
            InterceptResult::Cancelled { service, .. } => {
                assert_eq!(service, "notion");
            }
            other => panic!("expected Cancelled, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn intercept_normal_message_passes_through() {
        let vault: Arc<dyn SecretStore> = Arc::new(InMemoryVault::new());
        let mut gate = CredentialGate::new(Arc::clone(&vault), None);

        gate.register_pending(&Principal::Owner, make_prompt(Some("ntn_")));

        let event = make_event("What's the weather today?", Principal::Owner);
        let result = gate.intercept(&event).await;

        assert!(matches!(result, InterceptResult::NotIntercepted));
    }

    #[tokio::test]
    async fn intercept_ttl_expired() {
        let vault: Arc<dyn SecretStore> = Arc::new(InMemoryVault::new());
        let mut gate = CredentialGate::new(Arc::clone(&vault), None);

        let mut prompt = make_prompt(Some("ntn_"));
        prompt.ttl = Duration::from_millis(0); // Already expired.
        prompt.prompted_at = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or(Instant::now());
        gate.register_pending(&Principal::Owner, prompt);

        let event = make_event("ntn_265011509509ABCdefGHIjkl", Principal::Owner);
        let result = gate.intercept(&event).await;

        assert!(matches!(result, InterceptResult::NotIntercepted));
    }

    #[tokio::test]
    async fn intercept_no_pending_passes_through() {
        let vault: Arc<dyn SecretStore> = Arc::new(InMemoryVault::new());
        let mut gate = CredentialGate::new(Arc::clone(&vault), None);

        // No pending prompt registered.
        let event = make_event("ntn_265011509509ABCdefGHIjkl", Principal::Owner);
        let result = gate.intercept(&event).await;

        assert!(matches!(result, InterceptResult::NotIntercepted));
    }
}
