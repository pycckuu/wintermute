//! KernelFlowManager -- integration setup state machine
//! (feature-dynamic-integrations, spec 5.1, 8.5).
//!
//! Replaces CredentialGate. Setup commands ("connect notion") bypass the
//! pipeline entirely. The flow manager handles: prompt -> intercept token ->
//! store in vault -> delete message -> spawn MCP server -> report.
//! No LLM involved. Auto-continuation eliminates the dead-end.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::kernel::journal::TaskJournal;
use crate::kernel::vault::{SecretStore, SecretValue};
use crate::tools::mcp::manager::McpServerManager;
use crate::tools::mcp::{find_known_server, KnownServer, McpServerCommand, McpServerConfig};
use crate::types::{InboundEvent, Principal};

/// Default TTL for awaiting credential input (10 minutes, spec 8.5).
const FLOW_TTL_SECS: u64 = 600;

/// Flow states for the integration setup state machine
/// (feature-dynamic-integrations, spec 8.5).
#[derive(Debug)]
pub enum FlowState {
    /// Waiting for owner to paste credential.
    AwaitingCredential {
        /// When the prompt was shown.
        prompted_at: Instant,
        /// How long the flow stays active.
        ttl: Duration,
    },
    /// Spawning MCP server after credential stored.
    Spawning,
}

/// A single integration setup flow (feature-dynamic-integrations, spec 8.5).
#[derive(Debug)]
pub struct KernelFlow {
    /// Service name (e.g., "notion").
    pub service: String,
    /// Current state.
    pub state: FlowState,
    /// Expected token prefix for credential classification (e.g., "ntn_").
    pub expected_prefix: Option<String>,
    /// Vault storage key without "vault:" prefix (e.g., "notion_notion_token").
    pub vault_key: String,
}

/// Result of flow intercept on an inbound event
/// (feature-dynamic-integrations, spec 8.5).
#[derive(Debug)]
pub enum FlowIntercept {
    /// Flow consumed the message -- send response, optionally delete message.
    Consumed {
        /// Response text to send to the user.
        response: String,
        /// Optional (chat_id, message_id) pair for message deletion.
        delete_message: Option<(String, String)>,
    },
    /// Message was not consumed -- proceed with normal pipeline.
    NotConsumed,
}

/// Integration setup state machine -- replaces CredentialGate
/// (feature-dynamic-integrations, spec 5.1, 8.5, Invariant B).
///
/// Setup commands bypass the pipeline entirely. The flow manager handles
/// the full lifecycle: prompt -> intercept -> store -> spawn -> report.
pub struct KernelFlowManager {
    /// Active flows keyed by serialized principal.
    active_flows: HashMap<String, KernelFlow>,
    /// Vault for storing credentials.
    vault: Arc<dyn SecretStore>,
    /// MCP server manager for spawning servers.
    mcp_manager: Arc<McpServerManager>,
    /// Journal for crash recovery.
    journal: Option<Arc<TaskJournal>>,
}

impl KernelFlowManager {
    /// Create a new flow manager, restoring any persisted pending flows
    /// from the journal (feature-dynamic-integrations, spec 8.5).
    pub fn new(
        vault: Arc<dyn SecretStore>,
        mcp_manager: Arc<McpServerManager>,
        journal: Option<Arc<TaskJournal>>,
    ) -> Self {
        let mut active_flows = HashMap::new();

        if let Some(ref j) = journal {
            match j.load_all_pending_credential_prompts() {
                Ok(prompts) => {
                    for (principal_key, service, vault_key, expected_prefix) in prompts {
                        debug!(
                            principal = %principal_key,
                            service = %service,
                            "restored pending flow from journal"
                        );
                        active_flows.insert(
                            principal_key,
                            KernelFlow {
                                service,
                                state: FlowState::AwaitingCredential {
                                    prompted_at: Instant::now(),
                                    ttl: Duration::from_secs(FLOW_TTL_SECS),
                                },
                                expected_prefix,
                                vault_key,
                            },
                        );
                    }
                }
                Err(e) => {
                    warn!(error = %e, "failed to load pending flows from journal");
                }
            }
        }

        Self {
            active_flows,
            vault,
            mcp_manager,
            journal,
        }
    }

    /// Start a setup flow for a service (feature-dynamic-integrations, spec 8.1).
    ///
    /// Checks the known server registry, probes the vault for existing
    /// credentials, and either spawns directly or prompts the owner.
    /// Returns a response message for the user.
    pub async fn start_setup(&mut self, service: &str, principal: &Principal) -> Option<String> {
        let principal_key = serde_json::to_string(principal).unwrap_or_default();

        let known = match find_known_server(service) {
            Some(k) => k,
            None => {
                return Some(format!(
                    "I don't have a built-in configuration for \"{service}\". \
                     Known services: {}.\n\n\
                     Try one of these, or configure a custom MCP server.",
                    known_service_names()
                ));
            }
        };

        // Extract first credential requirement.
        let (env_name, instructions) = match known.credentials.first() {
            Some(&(env_name, instructions)) => (env_name, instructions),
            None => {
                // No credentials needed -- spawn directly.
                return Some(self.spawn_known_server(service, known).await);
            }
        };

        // Check if credential already exists in vault.
        let vault_key = format!("{service}_{}", env_name.to_lowercase());

        if self.vault.get_secret(&vault_key).await.is_ok() {
            info!(service = %service, "credential already in vault, spawning directly");
            return Some(self.spawn_known_server(service, known).await);
        }

        // No credential -- register flow and prompt owner.
        let expected_prefix = known.expected_prefix.map(|s| s.to_owned());

        // Remove any existing flow for this principal (restart).
        if self.active_flows.contains_key(&principal_key) {
            self.remove_flow(&principal_key);
        }

        let flow = KernelFlow {
            service: service.to_owned(),
            state: FlowState::AwaitingCredential {
                prompted_at: Instant::now(),
                ttl: Duration::from_secs(FLOW_TTL_SECS),
            },
            expected_prefix: expected_prefix.clone(),
            vault_key: vault_key.clone(),
        };

        // Persist to journal for crash recovery.
        if let Some(ref j) = self.journal {
            if let Err(e) = j.save_pending_credential_prompt(
                &principal_key,
                service,
                &vault_key,
                expected_prefix.as_deref(),
            ) {
                warn!(error = %e, "failed to persist pending flow to journal");
            }
        }

        self.active_flows.insert(principal_key, flow);
        info!(service = %service, "flow registered: awaiting credential");

        Some(format!(
            "To connect {service}, I need your API credential.\n\n\
             {instructions}\n\n\
             Paste the token here when ready, or say \"cancel\" to abort."
        ))
    }

    /// Register a credential flow from pipeline execution of `admin.prompt_credential`
    /// (spec 8.5).
    ///
    /// When the Planner calls `admin.prompt_credential` through the pipeline
    /// (instead of the user saying "connect X"), we still need the FlowManager
    /// to intercept the next message (credential paste). This method registers
    /// that flow so `intercept()` can catch it.
    pub fn register_credential_flow(
        &mut self,
        principal: &Principal,
        service: &str,
        vault_key: &str,
        expected_prefix: Option<String>,
    ) {
        let principal_key = serde_json::to_string(principal).unwrap_or_default();

        // Remove "vault:" prefix if present — FlowManager stores raw vault keys.
        let raw_vault_key = vault_key.strip_prefix("vault:").unwrap_or(vault_key);

        // Remove any existing flow for this principal.
        if self.active_flows.contains_key(&principal_key) {
            self.remove_flow(&principal_key);
        }

        let flow = KernelFlow {
            service: service.to_owned(),
            state: FlowState::AwaitingCredential {
                prompted_at: Instant::now(),
                ttl: Duration::from_secs(FLOW_TTL_SECS),
            },
            expected_prefix: expected_prefix.clone(),
            vault_key: raw_vault_key.to_owned(),
        };

        // Persist to journal for crash recovery.
        if let Some(ref j) = self.journal {
            if let Err(e) = j.save_pending_credential_prompt(
                &principal_key,
                service,
                raw_vault_key,
                expected_prefix.as_deref(),
            ) {
                warn!(error = %e, "failed to persist pipeline credential flow to journal");
            }
        }

        self.active_flows.insert(principal_key, flow);
        info!(
            service = %service,
            "flow registered from pipeline: awaiting credential"
        );
    }

    /// Try to intercept an inbound event as part of an active setup flow
    /// (feature-dynamic-integrations, spec 8.5, Invariant B).
    ///
    /// Returns `Consumed` if the message was handled by the flow manager,
    /// or `NotConsumed` to let it proceed through the normal pipeline.
    pub async fn intercept(&mut self, event: &InboundEvent) -> FlowIntercept {
        let principal_key = serde_json::to_string(&event.source.principal).unwrap_or_default();

        // Extract flow metadata without holding a borrow across async calls.
        let flow_info = self.get_active_flow_info(&principal_key);

        let (service, vault_key, expected_prefix) = match flow_info {
            Some(info) => info,
            None => return FlowIntercept::NotConsumed,
        };

        // Get message text.
        let text = match event.payload.text.as_deref() {
            Some(t) => t.trim(),
            None => return FlowIntercept::NotConsumed,
        };

        // Extract chat_id and message_id for deletion.
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

        // Check for cancel.
        let lower = text.to_lowercase();
        if matches!(lower.as_str(), "cancel" | "nevermind" | "skip" | "abort") {
            self.remove_flow(&principal_key);
            return FlowIntercept::Consumed {
                response: format!("{service} setup cancelled."),
                delete_message: None,
            };
        }

        // Check for credential.
        if classify_credential(text, expected_prefix.as_deref()) {
            // Store credential in vault -- never enters pipeline (Invariant B).
            if let Err(e) = self
                .vault
                .store_secret(&vault_key, SecretValue::new(text))
                .await
            {
                warn!(error = %e, service = %service, "failed to store credential in vault");
                self.remove_flow(&principal_key);
                return FlowIntercept::Consumed {
                    response: format!("Failed to store {service} credential: {e}"),
                    delete_message: None,
                };
            }

            info!(service = %service, "credential stored via flow manager (Invariant B preserved)");

            // Persist message deletion for crash recovery.
            if !chat_id.is_empty() && !message_id.is_empty() {
                if let Some(ref j) = self.journal {
                    let _ = j.save_pending_deletion(&chat_id, &message_id);
                }
            }

            // Auto-continue: advance to spawn MCP server.
            let response = self.advance_to_spawn(&principal_key).await;

            let delete = if !chat_id.is_empty() && !message_id.is_empty() {
                Some((chat_id, message_id))
            } else {
                None
            };

            return FlowIntercept::Consumed {
                response,
                delete_message: delete,
            };
        }

        // Normal message -- not consumed by flow.
        FlowIntercept::NotConsumed
    }

    /// Clean up expired flows (feature-dynamic-integrations, spec 8.5).
    pub fn tick(&mut self) {
        let expired_keys: Vec<String> = self
            .active_flows
            .iter()
            .filter(|(_, flow)| match &flow.state {
                FlowState::AwaitingCredential { prompted_at, ttl } => prompted_at.elapsed() > *ttl,
                _ => false,
            })
            .map(|(k, _)| k.clone())
            .collect();

        for key in &expired_keys {
            info!(key = %key, "removing expired flow");
            self.remove_flow(key);
        }
    }

    /// Extract active flow info without holding a borrow across async calls.
    /// Returns `None` if no active flow exists or if the flow has expired.
    fn get_active_flow_info(
        &mut self,
        principal_key: &str,
    ) -> Option<(String, String, Option<String>)> {
        // Two-phase: check expiry first, then extract info.
        let expired = {
            let flow = self.active_flows.get(principal_key)?;
            matches!(
                &flow.state,
                FlowState::AwaitingCredential { prompted_at, ttl }
                    if prompted_at.elapsed() > *ttl
            )
        };

        if expired {
            info!(principal = %principal_key, "flow expired during intercept");
            self.remove_flow(principal_key);
            return None;
        }

        let flow = self.active_flows.get(principal_key)?;
        match &flow.state {
            FlowState::AwaitingCredential { .. } => Some((
                flow.service.clone(),
                flow.vault_key.clone(),
                flow.expected_prefix.clone(),
            )),
            _ => None,
        }
    }

    /// Advance from credential-stored to spawn MCP server
    /// (feature-dynamic-integrations, auto-continuation).
    async fn advance_to_spawn(&mut self, principal_key: &str) -> String {
        let service = match self.active_flows.get_mut(principal_key) {
            Some(flow) => {
                flow.state = FlowState::Spawning;
                flow.service.clone()
            }
            None => return "Internal error: flow state lost.".to_owned(),
        };

        let known = match find_known_server(&service) {
            Some(k) => k,
            None => {
                self.remove_flow(principal_key);
                return format!(
                    "{service} credential stored securely. \
                     Configure the MCP server manually to complete setup."
                );
            }
        };

        let config = build_known_server_config(&service, known);

        match self.mcp_manager.spawn_server(&config).await {
            Ok(action_ids) => {
                self.remove_flow(principal_key);
                let tool_count = action_ids.len();
                if action_ids.is_empty() {
                    format!("{service} connected successfully (no tools discovered).")
                } else {
                    format!(
                        "{service} connected! Discovered {tool_count} tools: {}",
                        action_ids.join(", ")
                    )
                }
            }
            Err(e) => {
                self.remove_flow(principal_key);
                format!(
                    "{service} credential stored, but failed to start the server: {e}\n\
                     Try \"connect {service}\" again."
                )
            }
        }
    }

    /// Spawn a known MCP server directly (credential exists or not needed)
    /// (feature-dynamic-integrations).
    async fn spawn_known_server(&self, service: &str, known: &KnownServer) -> String {
        let config = build_known_server_config(service, known);

        match self.mcp_manager.spawn_server(&config).await {
            Ok(action_ids) => {
                let tool_count = action_ids.len();
                if action_ids.is_empty() {
                    format!("{service} connected successfully (no tools discovered).")
                } else {
                    format!(
                        "{service} connected! Discovered {tool_count} tools: {}",
                        action_ids.join(", ")
                    )
                }
            }
            Err(e) => format!("Failed to connect {service}: {e}"),
        }
    }

    /// Remove a flow by principal key and clean up journal.
    fn remove_flow(&mut self, principal_key: &str) {
        self.active_flows.remove(principal_key);
        if let Some(ref j) = self.journal {
            let _ = j.delete_pending_credential_prompt(principal_key);
        }
    }
}

/// Detect "connect/setup/add X" commands (feature-dynamic-integrations).
///
/// Returns the service name if the text matches a setup command pattern.
pub fn parse_connect_command(text: &str) -> Option<String> {
    let lower = text.trim().to_lowercase();

    for prefix in &["connect ", "setup ", "add ", "integrate "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let service = rest.trim();
            if !service.is_empty()
                && service.len() <= 50
                && service
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                return Some(service.to_owned());
            }
        }
    }

    None
}

/// Classify whether text looks like a credential token
/// (feature-dynamic-integrations, spec 8.5).
fn classify_credential(text: &str, expected_prefix: Option<&str>) -> bool {
    if let Some(prefix) = expected_prefix {
        if text.starts_with(prefix) {
            return true;
        }
    }
    looks_like_token(text)
}

/// Heuristic: does this text look like an API token?
/// (feature-dynamic-integrations, spec 8.5).
///
/// Criteria: length 15-500, no whitespace, >90% token characters
/// (alphanumeric, `-`, `_`, `.`, `+`, `/`, `=`).
fn looks_like_token(text: &str) -> bool {
    let len = text.len();
    if !(15..=500).contains(&len) {
        return false;
    }

    if text.contains(char::is_whitespace) {
        return false;
    }

    let token_chars = text
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '='))
        .count();

    token_chars
        .checked_mul(10)
        .is_some_and(|scaled| scaled >= len.saturating_mul(9))
}

/// Build `McpServerConfig` from a known server entry
/// (feature-dynamic-integrations).
fn build_known_server_config(service: &str, known: &KnownServer) -> McpServerConfig {
    let mut auth = HashMap::new();
    for &(env_name, _) in known.credentials {
        auth.insert(
            env_name.to_owned(),
            format!("vault:{service}_{}", env_name.to_lowercase()),
        );
    }

    McpServerConfig {
        name: service.to_owned(),
        description: format!("Known MCP server: {service}"),
        label: known.default_label.to_owned(),
        allowed_domains: known.domains.iter().map(|d| (*d).to_owned()).collect(),
        server: McpServerCommand {
            command: known.command.to_owned(),
            args: known.args.iter().map(|a| (*a).to_owned()).collect(),
        },
        auth,
    }
}

/// Comma-separated list of known service names.
fn known_service_names() -> String {
    crate::tools::mcp::KNOWN_MCP_SERVERS
        .iter()
        .map(|s| s.name)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::audit::AuditLogger;
    use crate::kernel::vault::InMemoryVault;
    use crate::tools::ToolRegistry;
    use crate::types::{EventKind, EventPayload, EventSource};
    use chrono::Utc;
    use std::io::{Cursor, Write};
    use std::sync::Mutex;
    use uuid::Uuid;

    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Cursor<Vec<u8>>>>);

    impl SharedBuf {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Cursor::new(Vec::new()))))
        }
    }

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("test lock").write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.lock().expect("test lock").flush()
        }
    }

    fn make_manager_and_vault() -> (Arc<McpServerManager>, Arc<dyn SecretStore>) {
        let vault: Arc<dyn SecretStore> = Arc::new(InMemoryVault::new());
        let registry = Arc::new(ToolRegistry::new());
        let audit = Arc::new(AuditLogger::from_writer(Box::new(SharedBuf::new())));
        let manager = Arc::new(McpServerManager::new(registry, Arc::clone(&vault), audit));
        (manager, vault)
    }

    fn make_flow_manager() -> (KernelFlowManager, Arc<dyn SecretStore>) {
        let (manager, vault) = make_manager_and_vault();
        let fm = KernelFlowManager::new(Arc::clone(&vault), manager, None);
        (fm, vault)
    }

    fn make_event(text: &str, principal: Principal) -> InboundEvent {
        InboundEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            source: EventSource {
                adapter: "telegram".to_owned(),
                principal,
            },
            kind: EventKind::Message,
            payload: EventPayload {
                text: Some(text.to_owned()),
                attachments: vec![],
                reply_to: None,
                metadata: serde_json::json!({
                    "chat_id": "12345",
                    "message_id": "42",
                }),
            },
        }
    }

    // -- parse_connect_command --

    #[test]
    fn parse_connect_notion() {
        assert_eq!(
            parse_connect_command("connect notion"),
            Some("notion".to_owned())
        );
    }

    #[test]
    fn parse_setup_github() {
        assert_eq!(
            parse_connect_command("Setup GitHub"),
            Some("github".to_owned())
        );
    }

    #[test]
    fn parse_add_slack() {
        assert_eq!(parse_connect_command("add slack"), Some("slack".to_owned()));
    }

    #[test]
    fn parse_connect_with_whitespace() {
        assert_eq!(
            parse_connect_command("  connect notion  "),
            Some("notion".to_owned())
        );
    }

    #[test]
    fn parse_normal_message_returns_none() {
        assert!(parse_connect_command("hello").is_none());
        assert!(parse_connect_command("what's for lunch?").is_none());
        assert!(parse_connect_command("check my email").is_none());
    }

    #[test]
    fn parse_connect_empty_service() {
        assert!(parse_connect_command("connect ").is_none());
        assert!(parse_connect_command("connect").is_none());
    }

    // -- looks_like_token --

    #[test]
    fn token_valid_prefixed() {
        assert!(looks_like_token("ghp_ABCDEFghijklmnopqrstuvwxyz123456"));
        assert!(looks_like_token("ntn_265011509509ABCdefGHIjkl"));
        assert!(looks_like_token("xoxb-123-456-abcdefghij"));
    }

    #[test]
    fn token_too_short() {
        assert!(!looks_like_token("abc123"));
    }

    #[test]
    fn token_has_spaces() {
        assert!(!looks_like_token("this is a normal sentence with spaces"));
    }

    #[test]
    fn token_boundary_15_chars() {
        assert!(looks_like_token("abcdefghij12345")); // exactly 15
        assert!(!looks_like_token("abcdefghij1234")); // 14
    }

    // -- classify_credential --

    #[test]
    fn classify_with_known_prefix() {
        assert!(classify_credential("ntn_265011509509ABCdef", Some("ntn_")));
    }

    #[test]
    fn classify_heuristic_no_prefix() {
        assert!(classify_credential("sk-abc123def456ghi789jkl012mno", None));
    }

    #[test]
    fn classify_normal_text() {
        assert!(!classify_credential("hello world", Some("ntn_")));
    }

    // -- start_setup --

    #[tokio::test]
    async fn start_setup_known_service_no_credential() {
        let (mut fm, _vault) = make_flow_manager();
        let response = fm.start_setup("notion", &Principal::Owner).await;

        assert!(response.is_some());
        let text = response.expect("checked");
        assert!(text.contains("notion"), "should mention the service");
        assert!(
            text.contains("integration"),
            "should contain setup instructions"
        );
        assert!(text.contains("cancel"), "should mention cancel option");
    }

    #[tokio::test]
    async fn start_setup_unknown_service() {
        let (mut fm, _vault) = make_flow_manager();
        let response = fm.start_setup("nonexistent", &Principal::Owner).await;

        assert!(response.is_some());
        let text = response.expect("checked");
        assert!(
            text.contains("don't have"),
            "should indicate unknown service"
        );
        assert!(text.contains("notion"), "should list known services");
    }

    // -- intercept --

    #[tokio::test]
    async fn intercept_no_active_flow() {
        let (mut fm, _vault) = make_flow_manager();
        let event = make_event("ntn_265011509509ABCdefGHIjkl", Principal::Owner);
        let result = fm.intercept(&event).await;
        assert!(matches!(result, FlowIntercept::NotConsumed));
    }

    #[tokio::test]
    async fn intercept_cancel_removes_flow() {
        let (mut fm, _vault) = make_flow_manager();
        let _ = fm.start_setup("notion", &Principal::Owner).await;

        let event = make_event("cancel", Principal::Owner);
        let result = fm.intercept(&event).await;

        match result {
            FlowIntercept::Consumed { response, .. } => {
                assert!(
                    response.contains("cancelled"),
                    "should indicate cancellation"
                );
            }
            FlowIntercept::NotConsumed => panic!("expected Consumed"),
        }
    }

    #[tokio::test]
    async fn intercept_credential_stores_in_vault() {
        let (mut fm, vault) = make_flow_manager();
        let _ = fm.start_setup("notion", &Principal::Owner).await;

        let event = make_event("ntn_265011509509ABCdefGHIjkl", Principal::Owner);
        let result = fm.intercept(&event).await;

        match result {
            FlowIntercept::Consumed {
                response,
                delete_message,
            } => {
                // Credential should be stored in vault.
                let stored = vault.get_secret("notion_notion_token").await;
                assert!(stored.is_ok(), "credential should be stored in vault");
                assert_eq!(
                    stored.expect("checked").expose(),
                    "ntn_265011509509ABCdefGHIjkl"
                );

                // Message should be deleted.
                assert!(delete_message.is_some(), "should request message deletion");

                // Response should mention the service (spawn will fail, but credential is stored).
                assert!(
                    response.contains("notion"),
                    "response should mention service"
                );
            }
            FlowIntercept::NotConsumed => panic!("expected Consumed"),
        }
    }

    #[tokio::test]
    async fn intercept_normal_message_not_consumed() {
        let (mut fm, _vault) = make_flow_manager();
        let _ = fm.start_setup("notion", &Principal::Owner).await;

        let event = make_event("What's for lunch?", Principal::Owner);
        let result = fm.intercept(&event).await;
        assert!(matches!(result, FlowIntercept::NotConsumed));
    }

    #[tokio::test]
    async fn intercept_expired_flow_not_consumed() {
        let (mut fm, _vault) = make_flow_manager();

        // Manually insert a flow with zero TTL (already expired).
        let principal_key = serde_json::to_string(&Principal::Owner).unwrap_or_default();
        fm.active_flows.insert(
            principal_key,
            KernelFlow {
                service: "notion".to_owned(),
                state: FlowState::AwaitingCredential {
                    prompted_at: Instant::now()
                        .checked_sub(Duration::from_secs(1))
                        .unwrap_or_else(Instant::now),
                    ttl: Duration::from_millis(0),
                },
                expected_prefix: Some("ntn_".to_owned()),
                vault_key: "notion_notion_token".to_owned(),
            },
        );

        let event = make_event("ntn_265011509509ABCdefGHIjkl", Principal::Owner);
        let result = fm.intercept(&event).await;
        assert!(matches!(result, FlowIntercept::NotConsumed));
    }

    // -- tick --

    #[test]
    fn tick_removes_expired_flows() {
        let (manager, vault) = make_manager_and_vault();
        let mut fm = KernelFlowManager::new(vault, manager, None);

        let principal_key = serde_json::to_string(&Principal::Owner).unwrap_or_default();
        fm.active_flows.insert(
            principal_key.clone(),
            KernelFlow {
                service: "notion".to_owned(),
                state: FlowState::AwaitingCredential {
                    prompted_at: Instant::now()
                        .checked_sub(Duration::from_secs(1))
                        .unwrap_or_else(Instant::now),
                    ttl: Duration::from_millis(0),
                },
                expected_prefix: None,
                vault_key: "notion_notion_token".to_owned(),
            },
        );

        assert!(fm.active_flows.contains_key(&principal_key));
        fm.tick();
        assert!(!fm.active_flows.contains_key(&principal_key));
    }

    #[test]
    fn tick_keeps_active_flows() {
        let (manager, vault) = make_manager_and_vault();
        let mut fm = KernelFlowManager::new(vault, manager, None);

        let principal_key = serde_json::to_string(&Principal::Owner).unwrap_or_default();
        fm.active_flows.insert(
            principal_key.clone(),
            KernelFlow {
                service: "github".to_owned(),
                state: FlowState::AwaitingCredential {
                    prompted_at: Instant::now(),
                    ttl: Duration::from_secs(FLOW_TTL_SECS),
                },
                expected_prefix: Some("ghp_".to_owned()),
                vault_key: "github_github_personal_access_token".to_owned(),
            },
        );

        fm.tick();
        assert!(
            fm.active_flows.contains_key(&principal_key),
            "active flow should not be removed"
        );
    }

    // -- register_credential_flow (pipeline bridge) --

    #[tokio::test]
    async fn register_credential_flow_enables_intercept() {
        let (mut fm, vault) = make_flow_manager();

        // No flow registered yet — credential paste should NOT be consumed.
        let event = make_event("ntn_265011509509ABCdefGHIjkl", Principal::Owner);
        let result = fm.intercept(&event).await;
        assert!(matches!(result, FlowIntercept::NotConsumed));

        // Register flow from pipeline (simulating admin.prompt_credential execution).
        fm.register_credential_flow(
            &Principal::Owner,
            "notion",
            "vault:notion_notion_token",
            Some("ntn_".to_owned()),
        );

        // Now the credential paste SHOULD be consumed.
        let event2 = make_event("ntn_265011509509ABCdefGHIjkl", Principal::Owner);
        let result2 = fm.intercept(&event2).await;
        match result2 {
            FlowIntercept::Consumed { delete_message, .. } => {
                // Credential should be stored in vault.
                let stored = vault.get_secret("notion_notion_token").await;
                assert!(stored.is_ok(), "credential should be stored in vault");
                // Message should be deleted (Invariant B).
                assert!(delete_message.is_some(), "should request message deletion");
            }
            FlowIntercept::NotConsumed => {
                panic!("expected Consumed after register_credential_flow")
            }
        }
    }

    #[test]
    fn register_credential_flow_strips_vault_prefix() {
        let (manager, vault) = make_manager_and_vault();
        let mut fm = KernelFlowManager::new(vault, manager, None);

        fm.register_credential_flow(
            &Principal::Owner,
            "notion",
            "vault:notion_notion_token",
            Some("ntn_".to_owned()),
        );

        let principal_key = serde_json::to_string(&Principal::Owner).unwrap_or_default();
        let flow = fm
            .active_flows
            .get(&principal_key)
            .expect("flow should exist");
        assert_eq!(
            flow.vault_key, "notion_notion_token",
            "vault: prefix should be stripped"
        );
        assert_eq!(flow.service, "notion");
        assert_eq!(flow.expected_prefix.as_deref(), Some("ntn_"));
    }
}
