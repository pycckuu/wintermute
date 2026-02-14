//! MCP server lifecycle manager (feature-dynamic-integrations).
//!
//! Spawns MCP servers as child processes, performs the MCP handshake,
//! discovers tools, and registers them in the PFAR tool registry.
//! Handles shutdown and tool deregistration.

use std::collections::HashMap;
use std::sync::Arc;

use thiserror::Error;
use tracing;

use crate::kernel::audit::AuditLogger;
use crate::kernel::vault::SecretStore;
use crate::tools::mcp::client::McpClient;
use crate::tools::mcp::tool::McpTool;
use crate::tools::mcp::{infer_semantics, McpServerConfig};
use crate::tools::{ToolAction, ToolRegistry};
use crate::types::SecurityLabel;

// ── Error types ──

/// Errors from MCP server lifecycle operations (feature-dynamic-integrations).
#[derive(Debug, Error)]
pub enum McpManagerError {
    /// Failed to resolve credentials from vault.
    #[error("credential resolution failed for {server}: {detail}")]
    CredentialError {
        /// Server name.
        server: String,
        /// Error detail.
        detail: String,
    },

    /// Failed to parse security label from config.
    #[error("invalid security label '{label}' for server {server}")]
    InvalidLabel {
        /// Server name.
        server: String,
        /// The invalid label string.
        label: String,
    },

    /// Failed to spawn the MCP server process.
    #[error("spawn failed for {server}: {detail}")]
    SpawnError {
        /// Server name.
        server: String,
        /// Error detail.
        detail: String,
    },

    /// MCP protocol error during handshake or tool discovery.
    #[error("MCP protocol error for {server}: {detail}")]
    ProtocolError {
        /// Server name.
        server: String,
        /// Error detail.
        detail: String,
    },

    /// Server is not running.
    #[error("server '{0}' is not running")]
    NotRunning(String),
}

// ── Running server state ──

/// A running MCP server with its child process and client.
struct RunningServer {
    /// Child process handle.
    child: tokio::process::Child,
    /// Tool names registered for this server (for cleanup).
    tool_name: String,
}

// ── McpServerManager ──

/// Manages MCP server lifecycle: spawn, discover, register, stop
/// (feature-dynamic-integrations).
pub struct McpServerManager {
    servers: tokio::sync::Mutex<HashMap<String, RunningServer>>,
    registry: Arc<ToolRegistry>,
    vault: Arc<dyn SecretStore>,
    audit: Arc<AuditLogger>,
}

impl McpServerManager {
    /// Create a new MCP server manager.
    pub fn new(
        registry: Arc<ToolRegistry>,
        vault: Arc<dyn SecretStore>,
        audit: Arc<AuditLogger>,
    ) -> Self {
        Self {
            servers: tokio::sync::Mutex::new(HashMap::new()),
            registry,
            vault,
            audit,
        }
    }

    /// Spawn an MCP server, perform handshake, discover tools, and register them
    /// (feature-dynamic-integrations).
    ///
    /// Returns the list of registered tool action IDs (e.g., `["notion.search", "notion.create_page"]`).
    pub async fn spawn_server(
        &self,
        config: &McpServerConfig,
    ) -> Result<Vec<String>, McpManagerError> {
        // 1. Parse security label.
        let label: SecurityLabel =
            config
                .label
                .parse()
                .map_err(|_| McpManagerError::InvalidLabel {
                    server: config.name.clone(),
                    label: config.label.clone(),
                })?;

        // 2. Resolve credentials from vault → env vars.
        let mut env_vars: HashMap<String, String> = HashMap::new();
        for (env_name, vault_ref) in &config.auth {
            let ref_id = vault_ref.strip_prefix("vault:").unwrap_or(vault_ref);
            let secret = self.vault.get_secret(ref_id).await.map_err(|e| {
                McpManagerError::CredentialError {
                    server: config.name.clone(),
                    detail: format!("{env_name}: {e}"),
                }
            })?;
            env_vars.insert(env_name.clone(), secret.expose().to_owned());
        }

        // 3. Build and spawn child process.
        //    On macOS: direct spawn. On Linux: bwrap sandbox (deferred to hardening phase).
        let mut cmd = tokio::process::Command::new(&config.server.command);
        cmd.args(&config.server.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        // Inject credentials as env vars.
        for (k, v) in &env_vars {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| McpManagerError::SpawnError {
            server: config.name.clone(),
            detail: e.to_string(),
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpManagerError::SpawnError {
                server: config.name.clone(),
                detail: "failed to capture stdin".into(),
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpManagerError::SpawnError {
                server: config.name.clone(),
                detail: "failed to capture stdout".into(),
            })?;

        // 4. MCP handshake: initialize → tools/list.
        let mut client = McpClient::new(stdin, stdout);

        client
            .initialize()
            .await
            .map_err(|e| McpManagerError::ProtocolError {
                server: config.name.clone(),
                detail: format!("initialize: {e}"),
            })?;

        let mcp_tools = client
            .list_tools()
            .await
            .map_err(|e| McpManagerError::ProtocolError {
                server: config.name.clone(),
                detail: format!("tools/list: {e}"),
            })?;

        // 5. Build ToolActions from discovered MCP tools.
        let actions: Vec<ToolAction> = mcp_tools
            .iter()
            .map(|t| ToolAction {
                id: format!("{}.{}", config.name, t.name),
                description: t.description.clone().unwrap_or_default(),
                semantics: infer_semantics(t),
                label_ceiling: label,
                args_schema: t.input_schema.clone(),
            })
            .collect();

        let action_ids: Vec<String> = actions.iter().map(|a| a.id.clone()).collect();

        // 6. Create McpTool and register in tool registry.
        let client_arc = Arc::new(tokio::sync::Mutex::new(client));
        let mcp_tool = McpTool::new(
            config.name.clone(),
            actions,
            label,
            config.allowed_domains.clone(),
            client_arc,
        );

        self.registry.register(Arc::new(mcp_tool));

        // 7. Track the running server.
        let mut servers = self.servers.lock().await;
        servers.insert(
            config.name.clone(),
            RunningServer {
                child,
                tool_name: config.name.clone(),
            },
        );

        tracing::info!(
            server = %config.name,
            tools = action_ids.len(),
            "MCP server spawned and tools registered"
        );

        // Audit log (best-effort).
        let _ = self.audit.log_violation(&format!(
            "mcp_server_spawned: {} ({} tools)",
            config.name,
            action_ids.len()
        ));

        Ok(action_ids)
    }

    /// Stop an MCP server and unregister its tools (feature-dynamic-integrations).
    pub async fn stop_server(&self, name: &str) -> Result<(), McpManagerError> {
        let mut servers = self.servers.lock().await;
        let mut server = servers
            .remove(name)
            .ok_or_else(|| McpManagerError::NotRunning(name.to_owned()))?;

        // Unregister tools from the registry.
        self.registry.unregister(&server.tool_name);

        // Kill the child process.
        let _ = server.child.kill().await;

        tracing::info!(server = %name, "MCP server stopped and tools unregistered");

        let _ = self
            .audit
            .log_violation(&format!("mcp_server_stopped: {name}"));

        Ok(())
    }

    /// Shut down all running MCP servers (feature-dynamic-integrations).
    ///
    /// Called during PFAR graceful shutdown.
    pub async fn shutdown_all(&self) {
        let mut servers = self.servers.lock().await;
        let names: Vec<String> = servers.keys().cloned().collect();

        for name in &names {
            if let Some(mut server) = servers.remove(name) {
                self.registry.unregister(&server.tool_name);
                let _ = server.child.kill().await;
                tracing::info!(server = %name, "MCP server shut down");
            }
        }
    }

    /// List names of all running MCP servers.
    pub async fn list_servers(&self) -> Vec<String> {
        let servers = self.servers.lock().await;
        servers.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::audit::AuditLogger;
    use crate::kernel::vault::{InMemoryVault, SecretValue};
    use std::io::Cursor;
    use std::sync::Mutex;

    /// Shared buffer for audit logger in tests.
    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Cursor<Vec<u8>>>>);

    impl SharedBuf {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Cursor::new(Vec::new()))))
        }
    }

    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("test lock").write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.lock().expect("test lock").flush()
        }
    }

    fn make_manager() -> (McpServerManager, Arc<dyn SecretStore>) {
        let vault: Arc<dyn SecretStore> = Arc::new(InMemoryVault::new());
        let registry = Arc::new(ToolRegistry::new());
        let audit = Arc::new(AuditLogger::from_writer(Box::new(SharedBuf::new())));
        let manager = McpServerManager::new(registry, Arc::clone(&vault), audit);
        (manager, vault)
    }

    fn make_manager_with_registry() -> (McpServerManager, Arc<ToolRegistry>, Arc<dyn SecretStore>) {
        let vault: Arc<dyn SecretStore> = Arc::new(InMemoryVault::new());
        let registry = Arc::new(ToolRegistry::new());
        let audit = Arc::new(AuditLogger::from_writer(Box::new(SharedBuf::new())));
        let manager = McpServerManager::new(Arc::clone(&registry), Arc::clone(&vault), audit);
        (manager, registry, vault)
    }

    #[tokio::test]
    async fn test_spawn_server_with_mock() {
        let (manager, registry, vault) = make_manager_with_registry();

        // Store a mock credential.
        vault
            .store_secret("test_token", SecretValue::new("secret123"))
            .await
            .expect("store");

        // Mock MCP server: responds to initialize, reads initialized notification, responds to tools/list.
        let config = McpServerConfig {
            name: "mockserver".into(),
            description: "Test MCP server".into(),
            label: "internal".into(),
            allowed_domains: vec!["api.example.com".into()],
            server: crate::tools::mcp::McpServerCommand {
                command: "bash".into(),
                args: vec![
                    "-c".into(),
                    concat!(
                        r#"read line; "#,
                        r#"echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"mock","version":"1.0"}}}'; "#,
                        r#"read line; "#,
                        r#"read line; "#,
                        r#"echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"search","description":"Search things","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}},{"name":"create","description":"Create something"}]}}'; "#,
                        r#"sleep 10; "#,
                    )
                    .into(),
                ],
            },
            auth: {
                let mut m = HashMap::new();
                m.insert("TEST_TOKEN".into(), "vault:test_token".into());
                m
            },
        };

        let action_ids = manager
            .spawn_server(&config)
            .await
            .expect("spawn should succeed");

        assert_eq!(action_ids.len(), 2);
        assert!(action_ids.contains(&"mockserver.search".to_owned()));
        assert!(action_ids.contains(&"mockserver.create".to_owned()));

        // Verify tools are in the registry.
        let tool = registry.get_tool_and_action("mockserver.search");
        assert!(tool.is_some(), "search action should be in registry");

        // Verify server is listed.
        let servers = manager.list_servers().await;
        assert_eq!(servers, vec!["mockserver"]);

        // Stop the server.
        manager
            .stop_server("mockserver")
            .await
            .expect("stop should succeed");

        // Verify tools are removed.
        let tool = registry.get_tool_and_action("mockserver.search");
        assert!(tool.is_none(), "search action should be removed after stop");

        // Verify server is no longer listed.
        let servers = manager.list_servers().await;
        assert!(servers.is_empty());
    }

    #[tokio::test]
    async fn test_spawn_server_invalid_label() {
        let (manager, _vault) = make_manager();

        let config = McpServerConfig {
            name: "bad".into(),
            description: String::new(),
            label: "nonexistent_label".into(),
            allowed_domains: vec![],
            server: crate::tools::mcp::McpServerCommand {
                command: "true".into(),
                args: vec![],
            },
            auth: HashMap::new(),
        };

        let err = manager
            .spawn_server(&config)
            .await
            .expect_err("should fail");
        match err {
            McpManagerError::InvalidLabel { server, label } => {
                assert_eq!(server, "bad");
                assert_eq!(label, "nonexistent_label");
            }
            other => panic!("expected InvalidLabel, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_spawn_server_missing_credential() {
        let (manager, _vault) = make_manager();

        let config = McpServerConfig {
            name: "nocred".into(),
            description: String::new(),
            label: "internal".into(),
            allowed_domains: vec![],
            server: crate::tools::mcp::McpServerCommand {
                command: "true".into(),
                args: vec![],
            },
            auth: {
                let mut m = HashMap::new();
                m.insert("API_KEY".into(), "vault:nonexistent_key".into());
                m
            },
        };

        let err = manager
            .spawn_server(&config)
            .await
            .expect_err("should fail");
        match err {
            McpManagerError::CredentialError { server, .. } => {
                assert_eq!(server, "nocred");
            }
            other => panic!("expected CredentialError, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_stop_nonexistent_server() {
        let (manager, _vault) = make_manager();

        let err = manager
            .stop_server("does_not_exist")
            .await
            .expect_err("should fail");
        match err {
            McpManagerError::NotRunning(name) => assert_eq!(name, "does_not_exist"),
            other => panic!("expected NotRunning, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_shutdown_all_empty() {
        let (manager, _vault) = make_manager();
        // Should not panic on empty server list.
        manager.shutdown_all().await;
        assert!(manager.list_servers().await.is_empty());
    }
}
