//! McpTool — Tool trait adapter for MCP servers (feature-dynamic-integrations).
//!
//! Routes `Tool::execute()` calls to an MCP server child process via
//! JSON-RPC over stdin/stdout. Each McpTool represents all actions
//! discovered from a single MCP server.

use std::sync::Arc;

use async_trait::async_trait;

use crate::tools::mcp::client::McpClient;
use crate::tools::scoped_http::ScopedHttpClient;
use crate::tools::{
    InjectedCredentials, Tool, ToolAction, ToolError, ToolManifest, ToolOutput, ValidatedCapability,
};
use crate::types::SecurityLabel;

/// Tool trait adapter routing execute() to an MCP server (feature-dynamic-integrations, spec 6.11).
///
/// Each McpTool wraps a single MCP server and exposes its discovered tools
/// as PFAR tool actions. The server name prefixes all action IDs
/// (e.g., `"notion.search"` for server `"notion"`, action `"search"`).
pub struct McpTool {
    /// MCP server name (used as tool name prefix).
    server_name: String,
    /// Actions discovered from MCP `tools/list`.
    actions: Vec<ToolAction>,
    /// Security label for all tool outputs (from server config).
    label: SecurityLabel,
    /// Network domains this server is allowed to contact.
    allowed_domains: Vec<String>,
    /// JSON-RPC client connected to the MCP server process.
    client: Arc<tokio::sync::Mutex<McpClient>>,
}

impl McpTool {
    /// Create a new McpTool from discovered MCP tools.
    pub fn new(
        server_name: String,
        actions: Vec<ToolAction>,
        label: SecurityLabel,
        allowed_domains: Vec<String>,
        client: Arc<tokio::sync::Mutex<McpClient>>,
    ) -> Self {
        Self {
            server_name,
            actions,
            label,
            allowed_domains,
            client,
        }
    }

    /// Extract the MCP action name from a fully qualified action ID.
    ///
    /// E.g., `"notion.search"` with server_name `"notion"` → `"search"`.
    fn mcp_action_name<'a>(&self, action_id: &'a str) -> Option<&'a str> {
        action_id
            .strip_prefix(&self.server_name)
            .and_then(|s| s.strip_prefix('.'))
    }
}

#[async_trait]
impl Tool for McpTool {
    fn manifest(&self) -> ToolManifest {
        ToolManifest {
            name: self.server_name.clone(),
            owner_only: false,
            actions: self.actions.clone(),
            network_allowlist: self.allowed_domains.clone(),
        }
    }

    /// Route execution to the MCP server via JSON-RPC `tools/call`
    /// (feature-dynamic-integrations).
    ///
    /// Strips the server name prefix from the action ID and sends the
    /// bare action name to the MCP server. Results are returned with
    /// `has_free_text: true` as a conservative default.
    async fn execute(
        &self,
        _cap: &ValidatedCapability,
        _creds: &InjectedCredentials,
        _http: &ScopedHttpClient,
        action: &str,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let mcp_name = self
            .mcp_action_name(action)
            .ok_or_else(|| ToolError::ActionNotFound(action.to_owned()))?;

        let mut client = self.client.lock().await;
        let result = client
            .call_tool(mcp_name, args)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("MCP call failed: {e}")))?;

        if result.is_error {
            let error_text = result
                .content
                .iter()
                .filter_map(|c| c.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n");
            return Err(ToolError::ExecutionFailed(error_text));
        }

        // Combine all text content blocks into a single result.
        let text_parts: Vec<&str> = result
            .content
            .iter()
            .filter_map(|c| c.text.as_deref())
            .collect();
        let combined = text_parts.join("\n");

        Ok(ToolOutput {
            data: serde_json::json!({
                "server": self.server_name,
                "action": mcp_name,
                "label": self.label,
                "result": combined,
            }),
            // Conservative default: MCP tool output may contain free text
            // from external services (spec 4.4).
            has_free_text: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::mcp::client::McpToolDef;
    use crate::tools::mcp::infer_semantics;
    use crate::tools::ActionSemantics;

    /// Build ToolActions from McpToolDefs as the manager would.
    fn build_actions(
        server_name: &str,
        tools: &[McpToolDef],
        label: SecurityLabel,
    ) -> Vec<ToolAction> {
        tools
            .iter()
            .map(|t| ToolAction {
                id: format!("{server_name}.{}", t.name),
                description: t.description.clone().unwrap_or_default(),
                semantics: infer_semantics(t),
                label_ceiling: label,
                args_schema: t.input_schema.clone(),
            })
            .collect()
    }

    #[tokio::test]
    async fn test_mcp_tool_manifest() {
        let tools = vec![
            McpToolDef {
                name: "search".into(),
                description: Some("Search pages".into()),
                input_schema: serde_json::json!({"type": "object"}),
                annotations: Default::default(),
            },
            McpToolDef {
                name: "create_page".into(),
                description: Some("Create a page".into()),
                input_schema: serde_json::json!({"type": "object"}),
                annotations: Default::default(),
            },
        ];

        let actions = build_actions("notion", &tools, SecurityLabel::Internal);
        let client = Arc::new(tokio::sync::Mutex::new(unsafe_mock_client()));
        let mcp_tool = McpTool::new(
            "notion".into(),
            actions,
            SecurityLabel::Internal,
            vec!["api.notion.com".into()],
            client,
        );

        let manifest = mcp_tool.manifest();
        assert_eq!(manifest.name, "notion");
        assert!(!manifest.owner_only);
        assert_eq!(manifest.actions.len(), 2);
        assert_eq!(manifest.actions[0].id, "notion.search");
        assert_eq!(manifest.actions[1].id, "notion.create_page");
        assert_eq!(manifest.network_allowlist, vec!["api.notion.com"]);
    }

    #[tokio::test]
    async fn test_mcp_action_name_extraction() {
        let client = Arc::new(tokio::sync::Mutex::new(unsafe_mock_client()));
        let tool = McpTool::new(
            "notion".into(),
            vec![],
            SecurityLabel::Internal,
            vec![],
            client,
        );

        assert_eq!(tool.mcp_action_name("notion.search"), Some("search"));
        assert_eq!(
            tool.mcp_action_name("notion.create_page"),
            Some("create_page")
        );
        assert_eq!(tool.mcp_action_name("github.search"), None);
        assert_eq!(tool.mcp_action_name("search"), None);
    }

    #[test]
    fn test_mcp_tool_actions_have_correct_semantics() {
        let tools = vec![
            McpToolDef {
                name: "search".into(),
                description: None,
                input_schema: serde_json::json!({}),
                annotations: crate::tools::mcp::client::McpToolAnnotations {
                    read_only_hint: Some(true),
                    destructive_hint: None,
                },
            },
            McpToolDef {
                name: "delete".into(),
                description: None,
                input_schema: serde_json::json!({}),
                annotations: crate::tools::mcp::client::McpToolAnnotations {
                    read_only_hint: None,
                    destructive_hint: Some(true),
                },
            },
            McpToolDef {
                name: "unknown".into(),
                description: None,
                input_schema: serde_json::json!({}),
                annotations: Default::default(),
            },
        ];

        let actions = build_actions("srv", &tools, SecurityLabel::Internal);
        assert_eq!(actions[0].semantics, ActionSemantics::Read);
        assert_eq!(actions[1].semantics, ActionSemantics::Write);
        assert_eq!(actions[2].semantics, ActionSemantics::Write); // default: write
    }

    /// Create a mock McpClient for manifest tests (never actually called).
    ///
    /// Uses a no-op child process. Only for sync tests that don't execute tools.
    fn unsafe_mock_client() -> McpClient {
        // Spawn a trivially short-lived process — we never actually read/write.
        let mut child = std::process::Command::new("true")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn true");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        // Wait to prevent zombie process (clippy::zombie_processes).
        let _ = child.wait();
        // Convert std handles to tokio handles.
        let tokio_stdin = tokio::process::ChildStdin::from_std(stdin).expect("tokio stdin");
        let tokio_stdout = tokio::process::ChildStdout::from_std(stdout).expect("tokio stdout");
        McpClient::new(tokio_stdin, tokio_stdout)
    }
}
