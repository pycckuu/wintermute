//! Tool modules — in-process SaaS integrations (spec 5.4, 6.10, 6.11).
//!
//! Each tool implements the [`Tool`] trait and receives only what the
//! kernel provides: a validated capability token, injected credentials,
//! a domain-scoped HTTP client, and validated arguments.
//!
//! Tools cannot access the vault, other tools, adapters, or kernel
//! internals directly — isolation is enforced by API design (spec 5.4).

pub mod admin;
pub mod calendar;
pub mod email;
pub mod scoped_http;

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::tools::scoped_http::HttpError;
use crate::types::{CapabilityToken, SecurityLabel};

// ── Action semantics ──

/// Whether a tool action reads data or writes/mutates it (spec 6.11).
///
/// The kernel uses this to decide taint-gated approval: write actions
/// with tainted arguments may require human approval (spec 4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionSemantics {
    /// Read-only action — no external side effects.
    Read,
    /// Write or mutation — may require approval if tainted.
    Write,
}

// ── Tool action descriptor ──

/// Describes a single action a tool can perform (spec 6.11, 18.4).
///
/// The kernel uses this to validate planner-generated plans against
/// the template's allowed tools list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolAction {
    /// Fully qualified action ID (e.g. "email.list", "calendar.freebusy").
    pub id: String,
    /// Human-readable description for the planner prompt.
    pub description: String,
    /// Read or write semantics.
    pub semantics: ActionSemantics,
    /// Kernel-defined label ceiling for data produced by this action (spec 6.2).
    pub label_ceiling: SecurityLabel,
    /// JSON Schema describing the expected arguments.
    pub args_schema: serde_json::Value,
}

// ── Tool manifest ──

/// Declares a tool's capabilities and constraints (spec 5.4).
///
/// Returned by [`Tool::manifest`]. The kernel uses this to build
/// the tool registry, validate plans, and create `ScopedHttpClient`
/// instances with the correct domain allowlist.
#[derive(Debug, Clone)]
pub struct ToolManifest {
    /// Tool name (e.g. "email", "calendar", "admin").
    pub name: String,
    /// If true, only `principal:owner` can invoke this tool (spec 8.2).
    pub owner_only: bool,
    /// Actions this tool supports.
    pub actions: Vec<ToolAction>,
    /// Domains this tool is allowed to contact (spec 16.3).
    pub network_allowlist: Vec<String>,
}

// ── Validated capability ──

/// A capability token validated by the kernel (spec 4.6).
///
/// Tools receive this as proof of authorization. Construction is
/// restricted to kernel code via `pub(crate)` visibility.
#[derive(Debug, Clone)]
pub struct ValidatedCapability {
    token: CapabilityToken,
}

impl ValidatedCapability {
    /// Create a validated capability (kernel-only) (spec 4.6).
    ///
    /// Only crate-internal code can construct this, preventing tools
    /// from forging capabilities.
    #[allow(dead_code)] // Used by kernel pipeline code in Phase 2.4+
    pub(crate) fn new(token: CapabilityToken) -> Self {
        Self { token }
    }

    /// Access the underlying capability token.
    pub fn token(&self) -> &CapabilityToken {
        &self.token
    }
}

// ── Injected credentials ──

/// Credentials injected by the kernel at tool invocation time (spec 5.4).
///
/// Tools receive resolved credential values — they never see vault
/// references or access the vault directly.
#[derive(Debug, Clone)]
pub struct InjectedCredentials {
    values: HashMap<String, String>,
}

impl InjectedCredentials {
    /// Create an empty credential set.
    pub fn new() -> Self {
        Self {
            values: HashMap::new(),
        }
    }

    /// Insert a credential (kernel-only) (spec 5.4).
    #[allow(dead_code)] // Used by kernel pipeline code in Phase 2.4+
    pub(crate) fn insert(&mut self, key: String, value: String) {
        self.values.insert(key, value);
    }

    /// Retrieve a credential value by key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|s| s.as_str())
    }
}

impl Default for InjectedCredentials {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tool output ──

/// Output returned by a tool execution (spec 10.6).
///
/// The `has_free_text` flag is critical for graduated taint rules:
/// if the output contains free-text content (not just structured
/// fields), it may require human approval before being written to
/// external sinks (spec 4.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    /// Structured output data.
    pub data: serde_json::Value,
    /// Whether the output contains free-text content (spec 4.4).
    ///
    /// `true` means the output has prose or user-generated text,
    /// which may carry injection risk and requires approval for
    /// external writes when tainted.
    pub has_free_text: bool,
}

// ── Tool error ──

/// Errors that can occur during tool execution (spec 5.4).
#[derive(Debug, Error)]
pub enum ToolError {
    /// Tool execution failed with a descriptive message.
    #[error("tool execution failed: {0}")]
    ExecutionFailed(String),
    /// A required credential was not provided by the kernel.
    #[error("missing credential: {0}")]
    MissingCredential(String),
    /// Arguments did not match the action's schema.
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    /// The requested action was not found in the tool's manifest.
    #[error("action not found: {0}")]
    ActionNotFound(String),
    /// HTTP request error from the scoped client.
    #[error("HTTP error: {0}")]
    HttpError(#[from] HttpError),
}

// ── Tool trait ──

/// In-process tool receiving only kernel-provided inputs (spec 5.4).
///
/// Tools implement this trait to provide SaaS integrations. They cannot
/// access the vault, other tools, adapters, or kernel internals —
/// isolation is enforced by API design.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Declare this tool's capabilities, label ceilings, and network needs (spec 5.4).
    fn manifest(&self) -> ToolManifest;

    /// Execute a single action (spec 5.4).
    ///
    /// The tool receives:
    /// - `cap`: validated capability token proving authorization
    /// - `creds`: resolved credentials (tool never sees vault refs)
    /// - `http`: HTTP client scoped to this tool's allowed domains
    /// - `action`: the specific action ID to execute
    /// - `args`: validated arguments matching the action's schema
    async fn execute(
        &self,
        cap: &ValidatedCapability,
        creds: &InjectedCredentials,
        http: &scoped_http::ScopedHttpClient,
        action: &str,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError>;
}

// ── Tool registry ──

/// Registry of tool modules indexed by name for dispatch (spec 5.4, 7).
///
/// The kernel uses this to look up tools when executing plans.
/// Actions are addressed as `"tool_name.action_name"` (e.g. `"email.list"`).
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Create an empty tool registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool. The tool's manifest `name` is used as the key.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.manifest().name.clone();
        self.tools.insert(name, tool);
    }

    /// Look up a tool and its action descriptor by fully qualified action ID (spec 5.4).
    ///
    /// The `action_id` format is `"tool_name.action_name"` (e.g. `"email.list"`).
    /// Returns the tool reference and the matching `ToolAction`, or `None` if
    /// either the tool or the action is not found.
    pub fn get_tool_and_action(&self, action_id: &str) -> Option<(&dyn Tool, ToolAction)> {
        let (tool_name, action_name) = action_id.split_once('.')?;
        let tool = self.tools.get(tool_name)?;
        let manifest = tool.manifest();
        let action = manifest
            .actions
            .into_iter()
            .find(|a| a.id == action_id || a.id.ends_with(&format!(".{action_name}")))?;
        Some((tool.as_ref(), action))
    }

    /// Return all actions from registered tools that are allowed by the template (spec 4.5).
    ///
    /// Filters actions against `allowed_tools` and `denied_tools` lists.
    /// Supports wildcard matching: `"admin.*"` matches all actions from the
    /// `admin` tool, and `"*"` matches everything.
    pub fn available_actions(&self, allowed: &[String], denied: &[String]) -> Vec<ToolAction> {
        let mut result = Vec::new();

        for tool in self.tools.values() {
            let manifest = tool.manifest();
            for action in &manifest.actions {
                if is_action_denied(&action.id, denied) {
                    continue;
                }
                if is_action_allowed(&action.id, allowed) {
                    result.push(action.clone());
                }
            }
        }

        result
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if an action ID matches a pattern (spec 4.5).
///
/// Supports:
/// - Exact match: `"email.list"` matches `"email.list"`
/// - Wildcard tool: `"admin.*"` matches `"admin.list_integrations"`
/// - Global wildcard: `"*"` matches everything
pub fn matches_pattern(action_id: &str, pattern: &str) -> bool {
    if pattern == action_id || pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        if let Some(tool_name) = action_id.split_once('.').map(|(t, _)| t) {
            return prefix == tool_name;
        }
    }
    false
}

/// Check if an action ID matches any entry in the allowed list (spec 4.5).
fn is_action_allowed(action_id: &str, allowed: &[String]) -> bool {
    allowed
        .iter()
        .any(|pattern| matches_pattern(action_id, pattern))
}

/// Check if an action ID matches any entry in the denied list (spec 4.5).
fn is_action_denied(action_id: &str, denied: &[String]) -> bool {
    denied
        .iter()
        .any(|pattern| matches_pattern(action_id, pattern))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ── Mock tool for testing ──

    struct MockTool {
        name: String,
        actions: Vec<ToolAction>,
    }

    impl MockTool {
        fn new(name: &str, actions: Vec<ToolAction>) -> Self {
            Self {
                name: name.to_owned(),
                actions,
            }
        }

        fn email_tool() -> Self {
            Self::new(
                "email",
                vec![
                    ToolAction {
                        id: "email.list".to_owned(),
                        description: "List recent emails".to_owned(),
                        semantics: ActionSemantics::Read,
                        label_ceiling: SecurityLabel::Sensitive,
                        args_schema: serde_json::json!({"account": "string"}),
                    },
                    ToolAction {
                        id: "email.read".to_owned(),
                        description: "Read a specific email".to_owned(),
                        semantics: ActionSemantics::Read,
                        label_ceiling: SecurityLabel::Sensitive,
                        args_schema: serde_json::json!({"message_id": "string"}),
                    },
                ],
            )
        }

        fn admin_tool() -> Self {
            Self::new(
                "admin",
                vec![
                    ToolAction {
                        id: "admin.list_integrations".to_owned(),
                        description: "List all integrations".to_owned(),
                        semantics: ActionSemantics::Read,
                        label_ceiling: SecurityLabel::Sensitive,
                        args_schema: serde_json::json!({}),
                    },
                    ToolAction {
                        id: "admin.activate_tool".to_owned(),
                        description: "Activate a tool module".to_owned(),
                        semantics: ActionSemantics::Write,
                        label_ceiling: SecurityLabel::Secret,
                        args_schema: serde_json::json!({"tool": "string"}),
                    },
                ],
            )
        }
    }

    #[async_trait]
    impl Tool for MockTool {
        fn manifest(&self) -> ToolManifest {
            ToolManifest {
                name: self.name.clone(),
                owner_only: self.name == "admin",
                actions: self.actions.clone(),
                network_allowlist: vec!["api.example.com".to_owned()],
            }
        }

        async fn execute(
            &self,
            _cap: &ValidatedCapability,
            _creds: &InjectedCredentials,
            _http: &scoped_http::ScopedHttpClient,
            action: &str,
            _args: serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            if self.actions.iter().any(|a| a.id == action) {
                Ok(ToolOutput {
                    data: serde_json::json!({"status": "ok", "action": action}),
                    has_free_text: false,
                })
            } else {
                Err(ToolError::ActionNotFound(action.to_owned()))
            }
        }
    }

    // ── ToolRegistry tests ──

    #[test]
    fn test_tool_registry_register_and_lookup() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockTool::email_tool()));

        let result = registry.get_tool_and_action("email.list");
        assert!(result.is_some(), "should find email.list");

        let (tool, action) = result.expect("already checked");
        assert_eq!(tool.manifest().name, "email");
        assert_eq!(action.id, "email.list");
        assert_eq!(action.semantics, ActionSemantics::Read);
    }

    #[test]
    fn test_tool_registry_missing_tool() {
        let registry = ToolRegistry::new();
        assert!(
            registry.get_tool_and_action("email.list").is_none(),
            "should return None for unregistered tool"
        );
    }

    #[test]
    fn test_tool_registry_missing_action() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockTool::email_tool()));

        assert!(
            registry.get_tool_and_action("email.send").is_none(),
            "should return None for unknown action on known tool"
        );
    }

    #[test]
    fn test_available_actions_filtering() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockTool::email_tool()));
        registry.register(Box::new(MockTool::admin_tool()));

        // Only allow email.list — should exclude email.read and all admin actions.
        let allowed = vec!["email.list".to_owned()];
        let denied: Vec<String> = vec![];
        let actions = registry.available_actions(&allowed, &denied);

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].id, "email.list");
    }

    #[test]
    fn test_available_actions_wildcard() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockTool::email_tool()));
        registry.register(Box::new(MockTool::admin_tool()));

        // Allow all admin actions via wildcard.
        let allowed = vec!["admin.*".to_owned()];
        let denied: Vec<String> = vec![];
        let actions = registry.available_actions(&allowed, &denied);

        assert_eq!(actions.len(), 2);
        assert!(actions.iter().any(|a| a.id == "admin.list_integrations"));
        assert!(actions.iter().any(|a| a.id == "admin.activate_tool"));
    }

    #[test]
    fn test_available_actions_denied_overrides() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockTool::email_tool()));

        // Allow all email actions but deny email.read.
        let allowed = vec!["email.*".to_owned()];
        let denied = vec!["email.read".to_owned()];
        let actions = registry.available_actions(&allowed, &denied);

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].id, "email.list");
    }

    #[test]
    fn test_available_actions_global_wildcard() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockTool::email_tool()));
        registry.register(Box::new(MockTool::admin_tool()));

        // Allow everything.
        let allowed = vec!["*".to_owned()];
        let denied: Vec<String> = vec![];
        let actions = registry.available_actions(&allowed, &denied);

        assert_eq!(actions.len(), 4);
    }

    #[test]
    fn test_available_actions_global_deny() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockTool::email_tool()));

        // Allow everything but deny everything.
        let allowed = vec!["*".to_owned()];
        let denied = vec!["*".to_owned()];
        let actions = registry.available_actions(&allowed, &denied);

        assert!(actions.is_empty());
    }

    // ── InjectedCredentials tests ──

    #[test]
    fn test_injected_credentials() {
        let mut creds = InjectedCredentials::new();
        creds.insert("api_key".to_owned(), "secret_123".to_owned());
        creds.insert("token".to_owned(), "tok_abc".to_owned());

        assert_eq!(creds.get("api_key"), Some("secret_123"));
        assert_eq!(creds.get("token"), Some("tok_abc"));
        assert_eq!(creds.get("missing"), None);
    }

    // ── ToolOutput serialization tests ──

    #[test]
    fn test_tool_output_serialization() {
        let output = ToolOutput {
            data: serde_json::json!({"emails": [{"id": "1", "subject": "hello"}]}),
            has_free_text: false,
        };

        let json = serde_json::to_string(&output).expect("should serialize");
        let deserialized: ToolOutput = serde_json::from_str(&json).expect("should deserialize");

        assert!(!deserialized.has_free_text);
        assert_eq!(
            deserialized.data["emails"][0]["subject"],
            serde_json::json!("hello")
        );
    }

    #[test]
    fn test_tool_output_with_free_text() {
        let output = ToolOutput {
            data: serde_json::json!({"summary": "Long free-text content here"}),
            has_free_text: true,
        };

        let json = serde_json::to_string(&output).expect("should serialize");
        let deserialized: ToolOutput = serde_json::from_str(&json).expect("should deserialize");

        assert!(deserialized.has_free_text);
    }

    // ── ValidatedCapability tests ──

    #[test]
    fn test_validated_capability_wraps_token() {
        use chrono::Utc;
        use uuid::Uuid;

        let token = CapabilityToken {
            capability_id: Uuid::new_v4(),
            task_id: Uuid::nil(),
            template_id: "test_template".to_owned(),
            principal: crate::types::Principal::Owner,
            tool: "email.list".to_owned(),
            resource_scope: "account:personal".to_owned(),
            taint_of_arguments: crate::types::TaintSet {
                level: crate::types::TaintLevel::Clean,
                origin: "owner".to_owned(),
                touched_by: vec![],
            },
            issued_at: Utc::now(),
            expires_at: Utc::now(),
            max_invocations: 1,
        };

        let cap = ValidatedCapability::new(token.clone());
        assert_eq!(cap.token().tool, "email.list");
        assert_eq!(cap.token().template_id, "test_template");
        assert_eq!(cap.token().task_id, Uuid::nil());
    }

    // ── Action matching helper tests ──

    #[test]
    fn test_is_action_allowed_exact() {
        let allowed = vec!["email.list".to_owned()];
        assert!(is_action_allowed("email.list", &allowed));
        assert!(!is_action_allowed("email.read", &allowed));
    }

    #[test]
    fn test_is_action_allowed_wildcard() {
        let allowed = vec!["admin.*".to_owned()];
        assert!(is_action_allowed("admin.list_integrations", &allowed));
        assert!(is_action_allowed("admin.activate_tool", &allowed));
        assert!(!is_action_allowed("email.list", &allowed));
    }

    #[test]
    fn test_is_action_allowed_global() {
        let allowed = vec!["*".to_owned()];
        assert!(is_action_allowed("anything.at.all", &allowed));
    }

    #[test]
    fn test_is_action_denied_exact() {
        let denied = vec!["email.send".to_owned()];
        assert!(is_action_denied("email.send", &denied));
        assert!(!is_action_denied("email.list", &denied));
    }

    // ── MockTool execution test ──

    #[tokio::test]
    async fn test_mock_tool_execute() {
        let tool = MockTool::email_tool();
        let token = CapabilityToken {
            capability_id: uuid::Uuid::new_v4(),
            task_id: uuid::Uuid::nil(),
            template_id: "test".to_owned(),
            principal: crate::types::Principal::Owner,
            tool: "email.list".to_owned(),
            resource_scope: "account:personal".to_owned(),
            taint_of_arguments: crate::types::TaintSet {
                level: crate::types::TaintLevel::Clean,
                origin: "owner".to_owned(),
                touched_by: vec![],
            },
            issued_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now(),
            max_invocations: 1,
        };
        let cap = ValidatedCapability::new(token);
        let creds = InjectedCredentials::new();
        let http = scoped_http::ScopedHttpClient::new(HashSet::new());

        let result = tool
            .execute(&cap, &creds, &http, "email.list", serde_json::json!({}))
            .await;
        assert!(result.is_ok());

        let output = result.expect("already checked");
        assert_eq!(output.data["status"], "ok");
        assert!(!output.has_free_text);
    }

    #[tokio::test]
    async fn test_mock_tool_execute_unknown_action() {
        let tool = MockTool::email_tool();
        let token = CapabilityToken {
            capability_id: uuid::Uuid::new_v4(),
            task_id: uuid::Uuid::nil(),
            template_id: "test".to_owned(),
            principal: crate::types::Principal::Owner,
            tool: "email.nonexistent".to_owned(),
            resource_scope: "account:personal".to_owned(),
            taint_of_arguments: crate::types::TaintSet {
                level: crate::types::TaintLevel::Clean,
                origin: "owner".to_owned(),
                touched_by: vec![],
            },
            issued_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now(),
            max_invocations: 1,
        };
        let cap = ValidatedCapability::new(token);
        let creds = InjectedCredentials::new();
        let http = scoped_http::ScopedHttpClient::new(HashSet::new());

        let result = tool
            .execute(
                &cap,
                &creds,
                &http,
                "email.nonexistent",
                serde_json::json!({}),
            )
            .await;
        assert!(matches!(result, Err(ToolError::ActionNotFound(_))));
    }
}
