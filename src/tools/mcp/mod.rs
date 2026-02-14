//! MCP server integration — dynamic tool discovery via Model Context Protocol
//! (feature-dynamic-integrations).
//!
//! Spawns MCP servers as child processes, discovers their tools via JSON-RPC,
//! and registers them in the PFAR tool pipeline. Preserves all privacy
//! invariants through sandbox isolation and label enforcement.

pub mod client;
pub mod manager;
pub mod tool;

use std::collections::HashMap;

use serde::Deserialize;

use crate::tools::mcp::client::McpToolDef;
use crate::tools::ActionSemantics;

// ── MCP Server Configuration (spec 3, feature-dynamic-integrations) ──

/// Configuration for a single MCP server, loaded from TOML
/// (feature-dynamic-integrations, spec 3).
///
/// One TOML file per server in `~/.pfar/mcp/`.
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    /// Server name (e.g., "notion", "github").
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Security label for all tool outputs (parsed via `SecurityLabel::from_str`).
    pub label: String,
    /// Domain allowlist for network access via proxy.
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Server command configuration.
    pub server: McpServerCommand,
    /// Credential env vars mapped to vault references (e.g., "NOTION_TOKEN" → "vault:notion_token").
    #[serde(default)]
    pub auth: HashMap<String, String>,
}

/// Command to spawn the MCP server process (feature-dynamic-integrations).
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerCommand {
    /// Executable (e.g., "node", "npx", "python").
    pub command: String,
    /// Arguments to pass to the command.
    #[serde(default)]
    pub args: Vec<String>,
}

// ── Known MCP Servers (spec 6.1, feature-dynamic-integrations) ──

/// A well-known MCP server from the built-in registry
/// (feature-dynamic-integrations, spec 6.1).
///
/// Used for conversational setup: owner says "Connect Notion" and
/// the agent looks up the known registry for install instructions.
#[derive(Debug, Clone)]
pub struct KnownServer {
    /// Server name (e.g., "notion").
    pub name: &'static str,
    /// npm package name (e.g., "@modelcontextprotocol/server-notion").
    pub package: &'static str,
    /// Network domains the server needs.
    pub domains: &'static [&'static str],
    /// Required credentials: (env var name, setup instructions).
    pub credentials: &'static [(&'static str, &'static str)],
    /// Default security label.
    pub default_label: &'static str,
}

/// Built-in registry of well-known MCP servers (feature-dynamic-integrations, spec 6.1).
///
/// Ships with templates for common services. The owner can reference these
/// during conversational setup ("Connect Notion") to auto-fill config.
pub const KNOWN_MCP_SERVERS: &[KnownServer] = &[
    KnownServer {
        name: "notion",
        package: "@modelcontextprotocol/server-notion",
        domains: &["api.notion.com"],
        credentials: &[(
            "NOTION_TOKEN",
            "Go to notion.so/profile/integrations -> Create integration -> Copy the Internal Integration Secret",
        )],
        default_label: "internal",
    },
    KnownServer {
        name: "github",
        package: "@modelcontextprotocol/server-github",
        domains: &["api.github.com"],
        credentials: &[(
            "GITHUB_PERSONAL_ACCESS_TOKEN",
            "Go to github.com/settings/tokens -> Fine-grained tokens -> Generate new token -> Copy",
        )],
        default_label: "internal",
    },
    KnownServer {
        name: "slack",
        package: "@modelcontextprotocol/server-slack",
        domains: &["slack.com", "api.slack.com"],
        credentials: &[(
            "SLACK_BOT_TOKEN",
            "Go to api.slack.com/apps -> Your app -> OAuth & Permissions -> Bot User OAuth Token",
        )],
        default_label: "internal",
    },
    KnownServer {
        name: "filesystem",
        package: "@modelcontextprotocol/server-filesystem",
        domains: &[],
        credentials: &[],
        default_label: "internal",
    },
    KnownServer {
        name: "fetch",
        package: "@modelcontextprotocol/server-fetch",
        domains: &[], // configured per-instance
        credentials: &[],
        default_label: "public",
    },
];

/// Look up a known MCP server by name (feature-dynamic-integrations).
pub fn find_known_server(name: &str) -> Option<&'static KnownServer> {
    KNOWN_MCP_SERVERS.iter().find(|s| s.name == name)
}

// ── Semantics Inference (spec 4.4, feature-dynamic-integrations) ──

/// Infer PFAR action semantics from MCP tool annotations
/// (feature-dynamic-integrations, spec 4.4).
///
/// Conservative default: if unannotated, assume write (triggers taint/approval).
pub fn infer_semantics(tool: &McpToolDef) -> ActionSemantics {
    match (
        tool.annotations.read_only_hint,
        tool.annotations.destructive_hint,
    ) {
        (Some(true), _) => ActionSemantics::Read,
        (_, Some(true)) => ActionSemantics::Write,
        _ => ActionSemantics::Write, // safe default
    }
}

// ── MCP Config Section (added to PfarConfig) ──

/// MCP subsystem configuration (feature-dynamic-integrations).
#[derive(Debug, Clone, Deserialize)]
pub struct McpConfig {
    /// Directory containing MCP server TOML configs.
    #[serde(default = "default_mcp_config_dir")]
    pub config_dir: String,
    /// Auto-start MCP servers on PFAR startup.
    #[serde(default = "default_true")]
    pub auto_start: bool,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            config_dir: default_mcp_config_dir(),
            auto_start: true,
        }
    }
}

fn default_mcp_config_dir() -> String {
    "~/.pfar/mcp".to_owned()
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::mcp::client::McpToolAnnotations;

    #[test]
    fn test_infer_semantics_read_only() {
        let tool = McpToolDef {
            name: "search".into(),
            description: None,
            input_schema: serde_json::json!({}),
            annotations: McpToolAnnotations {
                read_only_hint: Some(true),
                destructive_hint: None,
            },
        };
        assert_eq!(infer_semantics(&tool), ActionSemantics::Read);
    }

    #[test]
    fn test_infer_semantics_destructive() {
        let tool = McpToolDef {
            name: "delete".into(),
            description: None,
            input_schema: serde_json::json!({}),
            annotations: McpToolAnnotations {
                read_only_hint: None,
                destructive_hint: Some(true),
            },
        };
        assert_eq!(infer_semantics(&tool), ActionSemantics::Write);
    }

    #[test]
    fn test_infer_semantics_unannotated_defaults_to_write() {
        let tool = McpToolDef {
            name: "unknown".into(),
            description: None,
            input_schema: serde_json::json!({}),
            annotations: McpToolAnnotations {
                read_only_hint: None,
                destructive_hint: None,
            },
        };
        assert_eq!(infer_semantics(&tool), ActionSemantics::Write);
    }

    #[test]
    fn test_infer_semantics_both_read_and_destructive_prefers_read() {
        // Edge case: if both hints are set, read_only_hint takes priority.
        let tool = McpToolDef {
            name: "weird".into(),
            description: None,
            input_schema: serde_json::json!({}),
            annotations: McpToolAnnotations {
                read_only_hint: Some(true),
                destructive_hint: Some(true),
            },
        };
        assert_eq!(infer_semantics(&tool), ActionSemantics::Read);
    }

    #[test]
    fn test_find_known_server() {
        let notion = find_known_server("notion");
        assert!(notion.is_some());
        let notion = notion.expect("already checked");
        assert_eq!(notion.package, "@modelcontextprotocol/server-notion");
        assert_eq!(notion.domains, &["api.notion.com"]);
        assert_eq!(notion.credentials.len(), 1);
        assert_eq!(notion.credentials[0].0, "NOTION_TOKEN");
    }

    #[test]
    fn test_find_known_server_missing() {
        assert!(find_known_server("nonexistent").is_none());
    }

    #[test]
    fn test_known_servers_have_unique_names() {
        let names: Vec<&str> = KNOWN_MCP_SERVERS.iter().map(|s| s.name).collect();
        for (i, name) in names.iter().enumerate() {
            for (j, other) in names.iter().enumerate() {
                if i != j {
                    assert_ne!(name, other, "duplicate known server name: {name}");
                }
            }
        }
    }

    #[test]
    fn test_mcp_server_config_deserialize() {
        let toml_str = r#"
            name = "notion"
            description = "Notion workspace"
            label = "internal"
            allowed_domains = ["api.notion.com"]

            [server]
            command = "node"
            args = ["/path/to/server/index.js"]

            [auth]
            NOTION_TOKEN = "vault:notion_token"
        "#;

        let config: McpServerConfig = toml::from_str(toml_str).expect("should parse config");
        assert_eq!(config.name, "notion");
        assert_eq!(config.label, "internal");
        assert_eq!(config.allowed_domains, vec!["api.notion.com"]);
        assert_eq!(config.server.command, "node");
        assert_eq!(config.server.args, vec!["/path/to/server/index.js"]);
        assert_eq!(
            config.auth.get("NOTION_TOKEN").map(|s| s.as_str()),
            Some("vault:notion_token")
        );
    }

    #[test]
    fn test_mcp_server_config_minimal() {
        let toml_str = r#"
            name = "test"
            label = "public"

            [server]
            command = "echo"
        "#;

        let config: McpServerConfig =
            toml::from_str(toml_str).expect("should parse minimal config");
        assert_eq!(config.name, "test");
        assert_eq!(config.description, "");
        assert!(config.allowed_domains.is_empty());
        assert!(config.auth.is_empty());
        assert!(config.server.args.is_empty());
    }

    #[test]
    fn test_mcp_config_defaults() {
        let config = McpConfig::default();
        assert_eq!(config.config_dir, "~/.pfar/mcp");
        assert!(config.auto_start);
    }
}
