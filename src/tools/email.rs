//! Zoho Mail tool -- email list and read (spec 6.11, 12.2).
//!
//! Provides two read-only actions:
//! - `email.list` -- list recent emails (label ceiling: `Sensitive`)
//! - `email.read` -- read a specific email by message ID (label ceiling: `Sensitive`)
//!
//! Both actions use the Zoho Mail API with OAuth bearer token authentication.
//! Email bodies contain free text, so `email.read` marks `has_free_text: true`
//! to trigger graduated taint rules for downstream writes (spec 4.4).

use async_trait::async_trait;
use serde_json::json;
use tracing::instrument;

use super::{
    scoped_http::ScopedHttpClient, ActionSemantics, InjectedCredentials, Tool, ToolAction,
    ToolError, ToolManifest, ToolOutput, ValidatedCapability,
};
use crate::types::SecurityLabel;

/// Default number of emails to return when `limit` is not specified.
const DEFAULT_EMAIL_LIMIT: u64 = 10;

/// Maximum number of emails that can be requested in a single list call.
const MAX_EMAIL_LIMIT: u64 = 100;

/// Zoho Mail integration (spec 6.11).
///
/// Supports two actions:
/// - `email.list` -- list recent emails (structured metadata only)
/// - `email.read` -- read a specific email including body (contains free text)
pub struct EmailTool {
    manifest: ToolManifest,
}

impl EmailTool {
    /// Create a new `EmailTool` (spec 6.11).
    pub fn new() -> Self {
        Self {
            manifest: ToolManifest {
                name: "email".to_string(),
                owner_only: false,
                actions: vec![
                    ToolAction {
                        id: "email.list".to_string(),
                        description: "List recent emails".to_string(),
                        semantics: ActionSemantics::Read,
                        label_ceiling: SecurityLabel::Sensitive,
                        args_schema: json!({
                            "account": "string",
                            "limit": "integer (1-100, default 10)"
                        }),
                    },
                    ToolAction {
                        id: "email.read".to_string(),
                        description: "Read a specific email by message ID".to_string(),
                        semantics: ActionSemantics::Read,
                        label_ceiling: SecurityLabel::Sensitive,
                        args_schema: json!({
                            "message_id": "string"
                        }),
                    },
                ],
                network_allowlist: vec!["mail.zoho.eu".to_string()],
            },
        }
    }
}

impl Default for EmailTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for EmailTool {
    fn manifest(&self) -> ToolManifest {
        self.manifest.clone()
    }

    #[instrument(skip(self, _cap, creds, http, args), fields(tool = "email"))]
    async fn execute(
        &self,
        _cap: &ValidatedCapability,
        creds: &InjectedCredentials,
        http: &ScopedHttpClient,
        action: &str,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        match action {
            "email.list" => self.list_emails(creds, http, args).await,
            "email.read" => self.read_email(creds, http, args).await,
            other => Err(ToolError::ActionNotFound(other.to_string())),
        }
    }
}

impl EmailTool {
    /// List recent emails via Zoho Mail API (spec 6.11).
    ///
    /// Returns structured email metadata (id, from, subject, date) without
    /// full bodies, so `has_free_text` is `false`.
    async fn list_emails(
        &self,
        creds: &InjectedCredentials,
        http: &ScopedHttpClient,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        // Step 1: validate required arguments.
        let account = args
            .get("account")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("missing 'account' field".to_string()))?;

        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_EMAIL_LIMIT);

        // Clamp to safe maximum.
        let limit = limit.min(MAX_EMAIL_LIMIT);

        // Step 2: resolve API token from injected credentials.
        let token = creds
            .get("api_token")
            .ok_or_else(|| ToolError::MissingCredential("api_token".to_string()))?;

        // Step 3: call Zoho Mail API.
        let url =
            format!("https://mail.zoho.eu/api/accounts/{account}/messages/view?limit={limit}");

        let response = http
            .get_with_bearer(&url, token)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Zoho Mail API returned {status}: {body}"
            )));
        }

        // Step 4: parse and return structured output.
        let parsed: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        Ok(ToolOutput {
            data: parsed,
            has_free_text: false, // list returns metadata only, not full bodies
        })
    }

    /// Read a specific email by message ID via Zoho Mail API (spec 6.11).
    ///
    /// Returns the full email including body content. Since email bodies
    /// contain free text (potential injection vectors), `has_free_text` is
    /// `true`, triggering graduated taint rules for downstream writes (spec 4.4).
    async fn read_email(
        &self,
        creds: &InjectedCredentials,
        http: &ScopedHttpClient,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        // Step 1: validate required arguments.
        let message_id = args
            .get("message_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("missing 'message_id' field".to_string()))?;

        // Step 2: resolve API token from injected credentials.
        let token = creds
            .get("api_token")
            .ok_or_else(|| ToolError::MissingCredential("api_token".to_string()))?;

        // Step 3: call Zoho Mail API.
        // Uses "default" account ID; the kernel resolves the actual account
        // from credentials during capability issuance.
        let url = format!("https://mail.zoho.eu/api/accounts/default/messages/{message_id}");

        let response = http
            .get_with_bearer(&url, token)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Zoho Mail API returned {status}: {body}"
            )));
        }

        // Step 4: parse and return output with free-text flag.
        let parsed: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        Ok(ToolOutput {
            data: parsed,
            has_free_text: true, // email body contains free text (spec 4.4)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CapabilityToken, Principal, TaintLevel, TaintSet};
    use chrono::Utc;
    use std::collections::HashSet;
    use uuid::Uuid;

    /// Helper: create a ValidatedCapability for testing.
    fn test_capability(action: &str) -> ValidatedCapability {
        let token = CapabilityToken {
            capability_id: Uuid::new_v4(),
            task_id: Uuid::nil(),
            template_id: "test_template".to_owned(),
            principal: Principal::Owner,
            tool: action.to_owned(),
            resource_scope: "account:personal".to_owned(),
            taint_of_arguments: TaintSet {
                level: TaintLevel::Clean,
                origin: "owner".to_owned(),
                touched_by: vec![],
            },
            issued_at: Utc::now(),
            expires_at: Utc::now(),
            max_invocations: 1,
        };
        ValidatedCapability::new(token)
    }

    /// Helper: create a ScopedHttpClient with mail.zoho.eu allowed.
    fn test_http() -> ScopedHttpClient {
        let mut domains = HashSet::new();
        domains.insert("mail.zoho.eu".to_owned());
        ScopedHttpClient::new(domains)
    }

    #[test]
    fn test_email_manifest() {
        let tool = EmailTool::new();
        let manifest = tool.manifest();

        assert_eq!(manifest.name, "email");
        assert!(!manifest.owner_only);
        assert_eq!(manifest.actions.len(), 2);

        // Verify email.list action.
        let list_action = manifest
            .actions
            .iter()
            .find(|a| a.id == "email.list")
            .expect("email.list action should exist");
        assert_eq!(list_action.semantics, ActionSemantics::Read);
        assert_eq!(list_action.label_ceiling, SecurityLabel::Sensitive);

        // Verify email.read action.
        let read_action = manifest
            .actions
            .iter()
            .find(|a| a.id == "email.read")
            .expect("email.read action should exist");
        assert_eq!(read_action.semantics, ActionSemantics::Read);
        assert_eq!(read_action.label_ceiling, SecurityLabel::Sensitive);

        assert_eq!(manifest.network_allowlist, vec!["mail.zoho.eu"]);
    }

    #[test]
    fn test_email_default() {
        let tool = EmailTool::default();
        assert_eq!(tool.manifest().name, "email");
    }

    #[tokio::test]
    async fn test_email_list_missing_account() {
        let tool = EmailTool::new();
        let cap = test_capability("email.list");
        let creds = InjectedCredentials::new();
        let http = test_http();

        let result = tool
            .execute(&cap, &creds, &http, "email.list", json!({}))
            .await;

        assert!(
            matches!(result, Err(ToolError::InvalidArguments(ref msg)) if msg.contains("account"))
        );
    }

    #[tokio::test]
    async fn test_email_read_missing_message_id() {
        let tool = EmailTool::new();
        let cap = test_capability("email.read");
        let creds = InjectedCredentials::new();
        let http = test_http();

        let result = tool
            .execute(&cap, &creds, &http, "email.read", json!({}))
            .await;

        assert!(
            matches!(result, Err(ToolError::InvalidArguments(ref msg)) if msg.contains("message_id"))
        );
    }

    #[tokio::test]
    async fn test_email_list_missing_creds() {
        let tool = EmailTool::new();
        let cap = test_capability("email.list");
        let creds = InjectedCredentials::new(); // no api_token
        let http = test_http();

        let result = tool
            .execute(
                &cap,
                &creds,
                &http,
                "email.list",
                json!({"account": "personal"}),
            )
            .await;

        assert!(matches!(result, Err(ToolError::MissingCredential(ref key)) if key == "api_token"));
    }

    #[tokio::test]
    async fn test_email_read_missing_creds() {
        let tool = EmailTool::new();
        let cap = test_capability("email.read");
        let creds = InjectedCredentials::new(); // no api_token
        let http = test_http();

        let result = tool
            .execute(
                &cap,
                &creds,
                &http,
                "email.read",
                json!({"message_id": "msg_123"}),
            )
            .await;

        assert!(matches!(result, Err(ToolError::MissingCredential(ref key)) if key == "api_token"));
    }

    #[tokio::test]
    async fn test_email_action_not_found() {
        let tool = EmailTool::new();
        let cap = test_capability("email.send");
        let creds = InjectedCredentials::new();
        let http = test_http();

        let result = tool
            .execute(&cap, &creds, &http, "email.send", json!({}))
            .await;

        assert!(matches!(result, Err(ToolError::ActionNotFound(ref a)) if a == "email.send"));
    }

    #[tokio::test]
    async fn test_email_list_http_error_on_bad_domain() {
        let tool = EmailTool::new();
        let cap = test_capability("email.list");
        let mut creds = InjectedCredentials::new();
        creds.insert("api_token".to_owned(), "test_token".to_owned());
        // Empty allowlist -- the request should fail with domain not allowed.
        let http = ScopedHttpClient::new(HashSet::new());

        let result = tool
            .execute(
                &cap,
                &creds,
                &http,
                "email.list",
                json!({"account": "personal", "limit": 5}),
            )
            .await;

        // The scoped HTTP client should block the request (domain not allowed),
        // which gets wrapped in ToolError::ExecutionFailed.
        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }

    #[tokio::test]
    async fn test_email_read_http_error_on_bad_domain() {
        let tool = EmailTool::new();
        let cap = test_capability("email.read");
        let mut creds = InjectedCredentials::new();
        creds.insert("api_token".to_owned(), "test_token".to_owned());
        // Empty allowlist.
        let http = ScopedHttpClient::new(HashSet::new());

        let result = tool
            .execute(
                &cap,
                &creds,
                &http,
                "email.read",
                json!({"message_id": "msg_123"}),
            )
            .await;

        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }
}
