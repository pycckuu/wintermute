//! Google Calendar tool -- freebusy queries (spec 6.11, 12.2, 18.4).
//!
//! Provides a single read-only action: `calendar.freebusy`, which queries
//! the Google Calendar FreeBusy API. The freebusy endpoint returns only
//! busy/free time ranges (no event details), so its label ceiling is
//! `Internal` -- declassified per spec 4.3 for scheduling negotiation.

use async_trait::async_trait;
use serde_json::json;
use tracing::instrument;

use super::{
    scoped_http::ScopedHttpClient, ActionSemantics, InjectedCredentials, Tool, ToolAction,
    ToolError, ToolManifest, ToolOutput, ValidatedCapability,
};
use crate::types::SecurityLabel;

/// Maximum allowed range in hours for a freebusy query.
const MAX_RANGE_HOURS: u64 = 168; // 7 days

/// Default range in hours if not specified.
const DEFAULT_RANGE_HOURS: u64 = 8;

/// Google Calendar integration (spec 6.11).
///
/// Supports one action:
/// - `calendar.freebusy` -- get free/busy status for a date range (label ceiling: `Internal`)
pub struct CalendarTool {
    manifest: ToolManifest,
}

impl CalendarTool {
    /// Create a new `CalendarTool` (spec 6.11).
    pub fn new() -> Self {
        Self {
            manifest: ToolManifest {
                name: "calendar".to_string(),
                owner_only: false,
                actions: vec![ToolAction {
                    id: "calendar.freebusy".to_string(),
                    description: "Get free/busy status for a date range".to_string(),
                    semantics: ActionSemantics::Read,
                    label_ceiling: SecurityLabel::Internal, // declassified per spec 4.3
                    args_schema: json!({
                        "date": "string (YYYY-MM-DD)",
                        "range_hours": "integer (1-168, default 8)"
                    }),
                }],
                network_allowlist: vec!["www.googleapis.com".to_string()],
            },
        }
    }
}

impl Default for CalendarTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for CalendarTool {
    fn manifest(&self) -> ToolManifest {
        self.manifest.clone()
    }

    #[instrument(skip(self, _cap, creds, http, args), fields(tool = "calendar"))]
    async fn execute(
        &self,
        _cap: &ValidatedCapability,
        creds: &InjectedCredentials,
        http: &ScopedHttpClient,
        action: &str,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        match action {
            "calendar.freebusy" => self.freebusy(creds, http, args).await,
            other => Err(ToolError::ActionNotFound(other.to_string())),
        }
    }
}

impl CalendarTool {
    /// Query free/busy status via Google Calendar API (spec 6.11, 18.4).
    ///
    /// Calls `POST https://www.googleapis.com/calendar/v3/freeBusy` with
    /// the provided date and range. Returns structured busy/free time
    /// ranges (no event details).
    async fn freebusy(
        &self,
        creds: &InjectedCredentials,
        http: &ScopedHttpClient,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        // Step 1: validate required arguments.
        let date = args
            .get("date")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("missing 'date' field".to_string()))?;

        let range_hours = args
            .get("range_hours")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_RANGE_HOURS);

        // Clamp to a safe maximum to avoid unbounded queries.
        let range_hours = range_hours.min(MAX_RANGE_HOURS);

        // Step 2: resolve OAuth token from injected credentials.
        let token = creds
            .get("oauth_token")
            .ok_or_else(|| ToolError::MissingCredential("oauth_token".to_string()))?;

        // Step 3: build the API request body.
        let url = "https://www.googleapis.com/calendar/v3/freeBusy";
        let body = json!({
            "timeMin": format!("{date}T00:00:00Z"),
            "timeMax": format!("{date}T{range_hours:02}:00:00Z"),
            "items": [{"id": "primary"}]
        });

        // Step 4: send the request via the scoped HTTP client.
        let response = http
            .post_with_bearer(url, body, token)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let status = response.status();
        let response_body = response
            .text()
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Google Calendar API returned {status}: {response_body}"
            )));
        }

        // Step 5: parse and return structured output.
        let parsed: serde_json::Value = serde_json::from_str(&response_body)
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        Ok(ToolOutput {
            data: parsed,
            has_free_text: false, // freebusy returns only structured time ranges
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
            resource_scope: "calendar:primary".to_owned(),
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

    /// Helper: create a ScopedHttpClient with googleapis.com allowed.
    fn test_http() -> ScopedHttpClient {
        let mut domains = HashSet::new();
        domains.insert("www.googleapis.com".to_owned());
        ScopedHttpClient::new(domains)
    }

    #[test]
    fn test_calendar_manifest() {
        let tool = CalendarTool::new();
        let manifest = tool.manifest();

        assert_eq!(manifest.name, "calendar");
        assert!(!manifest.owner_only);
        assert_eq!(manifest.actions.len(), 1);

        let action = &manifest.actions[0];
        assert_eq!(action.id, "calendar.freebusy");
        assert_eq!(action.semantics, ActionSemantics::Read);
        assert_eq!(action.label_ceiling, SecurityLabel::Internal);

        assert_eq!(manifest.network_allowlist, vec!["www.googleapis.com"]);
    }

    #[test]
    fn test_calendar_default() {
        let tool = CalendarTool::default();
        assert_eq!(tool.manifest().name, "calendar");
    }

    #[tokio::test]
    async fn test_calendar_freebusy_missing_date() {
        let tool = CalendarTool::new();
        let cap = test_capability("calendar.freebusy");
        let creds = InjectedCredentials::new();
        let http = test_http();

        let result = tool
            .execute(&cap, &creds, &http, "calendar.freebusy", json!({}))
            .await;

        assert!(
            matches!(result, Err(ToolError::InvalidArguments(ref msg)) if msg.contains("date"))
        );
    }

    #[tokio::test]
    async fn test_calendar_freebusy_missing_creds() {
        let tool = CalendarTool::new();
        let cap = test_capability("calendar.freebusy");
        let creds = InjectedCredentials::new(); // no oauth_token
        let http = test_http();

        let result = tool
            .execute(
                &cap,
                &creds,
                &http,
                "calendar.freebusy",
                json!({"date": "2025-01-15"}),
            )
            .await;

        assert!(
            matches!(result, Err(ToolError::MissingCredential(ref key)) if key == "oauth_token")
        );
    }

    #[tokio::test]
    async fn test_calendar_action_not_found() {
        let tool = CalendarTool::new();
        let cap = test_capability("calendar.unknown");
        let creds = InjectedCredentials::new();
        let http = test_http();

        let result = tool
            .execute(&cap, &creds, &http, "calendar.unknown", json!({}))
            .await;

        assert!(matches!(result, Err(ToolError::ActionNotFound(ref a)) if a == "calendar.unknown"));
    }

    #[tokio::test]
    async fn test_calendar_freebusy_http_error_on_bad_domain() {
        let tool = CalendarTool::new();
        let cap = test_capability("calendar.freebusy");
        let mut creds = InjectedCredentials::new();
        creds.insert("oauth_token".to_owned(), "test_token".to_owned());
        // Empty allowlist -- the request should fail with domain not allowed.
        let http = ScopedHttpClient::new(HashSet::new());

        let result = tool
            .execute(
                &cap,
                &creds,
                &http,
                "calendar.freebusy",
                json!({"date": "2025-01-15", "range_hours": 8}),
            )
            .await;

        // The scoped HTTP client should block the request (domain not allowed),
        // which gets wrapped in ToolError::ExecutionFailed.
        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }
}
