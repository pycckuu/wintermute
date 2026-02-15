//! Admin tool for conversational configuration (spec 8.2).
//!
//! Privileged tool that allows the owner to manage integrations,
//! credentials, and templates through natural conversation. Only
//! `principal:owner` can invoke admin actions — enforced by the
//! executor's `owner_only` check (regression test 15).
//!
//! Unlike regular tools, AdminTool holds `Arc` references to kernel
//! components (vault, tool registry, template registry). This is
//! architecturally correct: AdminTool is part of the trusted computing
//! base, not an external integration (spec 5.2).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::kernel::template::TemplateRegistry;
use crate::kernel::vault::{SecretStore, SecretValue};
use crate::tools::mcp::manager::McpServerManager;
use crate::tools::mcp::{find_known_server, McpServerCommand, McpServerConfig};
use crate::tools::scoped_http::ScopedHttpClient;
use crate::tools::{
    ActionSemantics, InjectedCredentials, Tool, ToolAction, ToolError, ToolManifest, ToolOutput,
    ToolRegistry, ValidatedCapability,
};
use crate::types::SecurityLabel;

/// Admin tool for conversational configuration (spec 8.2).
///
/// Holds `Arc` references to kernel components, accessible only to
/// `principal:owner`. All actions carry `label_ceiling: Sensitive`
/// since admin outputs never contain raw secret values.
pub struct AdminTool {
    vault: Arc<dyn SecretStore>,
    tools: Arc<ToolRegistry>,
    templates: Arc<TemplateRegistry>,
    mcp_manager: Option<Arc<McpServerManager>>,
    /// Skills directory path for listing local skills
    /// (feature-self-extending-skills, spec 14).
    skills_dir: Option<String>,
}

impl AdminTool {
    /// Create a new admin tool with references to kernel components (spec 8.2).
    pub fn new(
        vault: Arc<dyn SecretStore>,
        tools: Arc<ToolRegistry>,
        templates: Arc<TemplateRegistry>,
    ) -> Self {
        Self {
            vault,
            tools,
            templates,
            mcp_manager: None,
            skills_dir: None,
        }
    }

    /// Set the MCP server manager for dynamic integration support
    /// (feature-dynamic-integrations).
    pub fn with_mcp_manager(mut self, manager: Arc<McpServerManager>) -> Self {
        self.mcp_manager = Some(manager);
        self
    }

    /// Set the skills directory for listing local skills
    /// (feature-self-extending-skills, spec 14).
    pub fn with_skills_dir(mut self, dir: String) -> Self {
        self.skills_dir = Some(dir);
        self
    }

    /// List all registered tool integrations (spec 8.2).
    fn list_integrations(&self) -> Result<ToolOutput, ToolError> {
        // Get all actions from the base tool registry (excludes admin itself).
        let all_allowed = vec!["*".to_owned()];
        let no_denied: Vec<String> = vec![];
        let actions = self.tools.available_actions(&all_allowed, &no_denied);

        // Group actions by tool name.
        let mut tools_map: std::collections::HashMap<String, Vec<serde_json::Value>> =
            std::collections::HashMap::new();
        for action in &actions {
            let tool_name = action
                .id
                .split_once('.')
                .map(|(name, _)| name)
                .unwrap_or(&action.id);
            tools_map
                .entry(tool_name.to_owned())
                .or_default()
                .push(json!({
                    "id": action.id,
                    "description": action.description,
                    "semantics": format!("{:?}", action.semantics),
                }));
        }

        let tools_list: Vec<serde_json::Value> = tools_map
            .into_iter()
            .map(|(name, actions)| {
                json!({
                    "name": name,
                    "action_count": actions.len(),
                    "actions": actions,
                })
            })
            .collect();

        Ok(ToolOutput {
            data: json!({ "tools": tools_list }),
            has_free_text: false,
        })
    }

    /// Check if a specific integration exists (spec 8.2).
    fn check_integration(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let service = args
            .get("service")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("missing 'service' argument".to_owned()))?;

        // Check if any actions exist for this service.
        let pattern = format!("{service}.*");
        let allowed = vec![pattern];
        let no_denied: Vec<String> = vec![];
        let actions = self.tools.available_actions(&allowed, &no_denied);

        if actions.is_empty() {
            Ok(ToolOutput {
                data: json!({
                    "exists": false,
                    "service": service,
                }),
                has_free_text: false,
            })
        } else {
            let action_list: Vec<serde_json::Value> = actions
                .iter()
                .map(|a| {
                    json!({
                        "id": a.id,
                        "description": a.description,
                    })
                })
                .collect();

            Ok(ToolOutput {
                data: json!({
                    "exists": true,
                    "service": service,
                    "actions": action_list,
                    "action_count": action_list.len(),
                }),
                has_free_text: false,
            })
        }
    }

    /// List all registered task templates (spec 8.2).
    fn list_templates(&self) -> Result<ToolOutput, ToolError> {
        let templates: Vec<serde_json::Value> = self
            .templates
            .list_all()
            .iter()
            .map(|t| {
                json!({
                    "template_id": t.template_id,
                    "triggers": t.triggers,
                    "principal_class": format!("{:?}", t.principal_class),
                    "description": t.description,
                })
            })
            .collect();

        Ok(ToolOutput {
            data: json!({ "templates": templates }),
            has_free_text: false,
        })
    }

    /// Return system status summary (spec 8.2).
    fn system_status(&self) -> Result<ToolOutput, ToolError> {
        let all_allowed = vec!["*".to_owned()];
        let no_denied: Vec<String> = vec![];
        let tool_actions = self.tools.available_actions(&all_allowed, &no_denied);
        let templates = self.templates.list_all();

        Ok(ToolOutput {
            data: json!({
                "tools_action_count": tool_actions.len(),
                "templates_count": templates.len(),
            }),
            has_free_text: false,
        })
    }

    /// Return credential prompt instructions for a service (spec 8.5).
    ///
    /// For known services (notion, github, slack), auto-fills credential
    /// type and setup instructions from the built-in registry. For unknown
    /// services, uses args or generic defaults.
    ///
    /// This is a Read action that returns structured instructions. The
    /// synthesizer (Phase 3) turns this into a natural language message
    /// asking the owner to provide the credential.
    fn prompt_credential(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let service = args
            .get("service")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("missing 'service' argument".to_owned()))?;

        // Auto-fill from known server registry (feature-dynamic-integrations).
        let known = find_known_server(service);

        let (credential_type, instructions, ref_id) = if let Some(known) = known {
            if let Some(&(env_name, setup_instructions)) = known.credentials.first() {
                let cred_type = env_name.to_lowercase();
                let vault_ref = format!("vault:{service}_{}", env_name.to_lowercase());
                (cred_type, setup_instructions.to_owned(), vault_ref)
            } else {
                // Known server with no credentials needed.
                return Ok(ToolOutput {
                    data: json!({
                        "prompt_required": false,
                        "service": service,
                        "message": format!("{service} does not require credentials. You can connect it directly with admin.connect_mcp_server."),
                    }),
                    has_free_text: false,
                });
            }
        } else {
            // Unknown service — use args or defaults.
            let cred_type = args
                .get("credential_type")
                .and_then(|v| v.as_str())
                .unwrap_or("api_token")
                .to_owned();
            let instr = args
                .get("instructions")
                .and_then(|v| v.as_str())
                .unwrap_or("Please provide the API credential for this service.")
                .to_owned();
            let vault_ref = format!("vault:{service}_{cred_type}");
            (cred_type, instr, vault_ref)
        };

        // Include expected_prefix from known server registry for credential gate
        // classification (feature-credential-acquisition, spec 8.5).
        let expected_prefix = known.and_then(|k| k.expected_prefix);

        Ok(ToolOutput {
            data: json!({
                "prompt_required": true,
                "service": service,
                "credential_type": credential_type,
                "instructions": instructions,
                "ref_id": ref_id,
                "expected_prefix": expected_prefix,
            }),
            has_free_text: false,
        })
    }

    /// List running MCP servers (feature-dynamic-integrations).
    async fn list_mcp_servers(&self) -> Result<ToolOutput, ToolError> {
        let manager = self
            .mcp_manager
            .as_ref()
            .ok_or_else(|| ToolError::ExecutionFailed("MCP manager not configured".into()))?;

        let servers = manager.list_servers().await;

        Ok(ToolOutput {
            data: json!({
                "servers": servers,
                "count": servers.len(),
            }),
            has_free_text: false,
        })
    }

    /// Connect an MCP server (feature-dynamic-integrations).
    ///
    /// Accepts either a known server name (looked up in the built-in registry)
    /// or a custom config with command and args.
    async fn connect_mcp_server(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let manager = self
            .mcp_manager
            .as_ref()
            .ok_or_else(|| ToolError::ExecutionFailed("MCP manager not configured".into()))?;

        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("missing 'name' argument".into()))?;

        // Check if this is a known server.
        let config = if let Some(known) = find_known_server(name) {
            // Use known server's command/args, allow override from args.
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or(known.command)
                .to_owned();
            let default_args = args
                .get("args")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_owned()))
                        .collect()
                })
                .unwrap_or_else(|| known.args.iter().map(|a| (*a).to_owned()).collect());

            let mut auth = std::collections::HashMap::new();
            for (env_name, _instructions) in known.credentials {
                auth.insert(
                    (*env_name).to_owned(),
                    format!("vault:{name}_{}", env_name.to_lowercase()),
                );
            }

            McpServerConfig {
                name: name.to_owned(),
                description: format!("Known MCP server: {name}"),
                label: known.default_label.to_owned(),
                allowed_domains: known.domains.iter().map(|d| (*d).to_owned()).collect(),
                server: McpServerCommand {
                    command,
                    args: default_args,
                },
                auth,
            }
        } else {
            // Custom server — require command and label.
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ToolError::InvalidArguments(
                        "missing 'command' for unknown server (not in known registry)".into(),
                    )
                })?
                .to_owned();

            let cmd_args: Vec<String> = args
                .get("args")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_owned()))
                        .collect()
                })
                .unwrap_or_default();

            let label = args
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("internal")
                .to_owned();

            let allowed_domains: Vec<String> = args
                .get("allowed_domains")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_owned()))
                        .collect()
                })
                .unwrap_or_default();

            McpServerConfig {
                name: name.to_owned(),
                description: format!("Custom MCP server: {name}"),
                label,
                allowed_domains,
                server: McpServerCommand {
                    command,
                    args: cmd_args,
                },
                auth: std::collections::HashMap::new(),
            }
        };

        let action_ids = manager
            .spawn_server(&config)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to connect: {e}")))?;

        Ok(ToolOutput {
            data: json!({
                "connected": true,
                "server": name,
                "tools_discovered": action_ids.len(),
                "tool_ids": action_ids,
            }),
            has_free_text: false,
        })
    }

    /// Disconnect an MCP server (feature-dynamic-integrations).
    async fn disconnect_mcp_server(
        &self,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let manager = self
            .mcp_manager
            .as_ref()
            .ok_or_else(|| ToolError::ExecutionFailed("MCP manager not configured".into()))?;

        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("missing 'name' argument".into()))?;

        manager
            .stop_server(name)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to disconnect: {e}")))?;

        Ok(ToolOutput {
            data: json!({
                "disconnected": true,
                "server": name,
            }),
            has_free_text: false,
        })
    }

    /// List locally deployed skills (feature-self-extending-skills, spec 14).
    ///
    /// Uses `spawn_blocking` since `find_local_skills` performs synchronous
    /// filesystem I/O (directory scanning).
    async fn list_skills(&self) -> Result<ToolOutput, ToolError> {
        let dir = self
            .skills_dir
            .as_deref()
            .ok_or_else(|| ToolError::ExecutionFailed("skills directory not configured".into()))?
            .to_owned();

        let skills =
            tokio::task::spawn_blocking(move || crate::tools::mcp::skills::find_local_skills(&dir))
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("skill scan failed: {e}")))?;

        let skill_list: Vec<serde_json::Value> = skills
            .iter()
            .map(|(config, path)| {
                json!({
                    "name": config.name,
                    "description": config.description,
                    "version": config.version,
                    "label": config.label,
                    "created_by": config.created_by,
                    "path": path.display().to_string(),
                    "status": "on_disk",
                })
            })
            .collect();

        Ok(ToolOutput {
            data: json!({
                "skills": skill_list,
                "count": skill_list.len(),
            }),
            has_free_text: false,
        })
    }

    /// Store a credential in the vault (spec 8.5).
    ///
    /// Write action — stores the provided value in the vault under the
    /// given ref_id. The value is wrapped in `SecretValue` which redacts
    /// itself in Debug output (invariant B).
    async fn store_credential(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let ref_id = args
            .get("ref_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("missing 'ref_id' argument".to_owned()))?;

        let value = args
            .get("value")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("missing 'value' argument".to_owned()))?;

        self.vault
            .store_secret(ref_id, SecretValue::new(value))
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        // Return confirmation without echoing the secret value (invariant B).
        Ok(ToolOutput {
            data: json!({
                "stored": true,
                "ref_id": ref_id,
            }),
            has_free_text: false,
        })
    }
}

#[async_trait]
impl Tool for AdminTool {
    /// Declare admin tool manifest (spec 8.2).
    fn manifest(&self) -> ToolManifest {
        ToolManifest {
            name: "admin".to_owned(),
            owner_only: true,
            actions: vec![
                ToolAction {
                    id: "admin.list_integrations".to_owned(),
                    description: "List all active tool integrations and their available actions"
                        .to_owned(),
                    semantics: ActionSemantics::Read,
                    label_ceiling: SecurityLabel::Sensitive,
                    args_schema: json!({}),
                },
                ToolAction {
                    id: "admin.check_integration".to_owned(),
                    description:
                        "Check if a built-in integration module exists and list its actions"
                            .to_owned(),
                    semantics: ActionSemantics::Read,
                    label_ceiling: SecurityLabel::Sensitive,
                    args_schema: json!({"service": "string (e.g. 'email', 'calendar', 'notion')"}),
                },
                ToolAction {
                    id: "admin.list_templates".to_owned(),
                    description: "List all task templates".to_owned(),
                    semantics: ActionSemantics::Read,
                    label_ceiling: SecurityLabel::Sensitive,
                    args_schema: json!({}),
                },
                ToolAction {
                    id: "admin.system_status".to_owned(),
                    description: "Show active tools, templates, and system health".to_owned(),
                    semantics: ActionSemantics::Read,
                    label_ceiling: SecurityLabel::Sensitive,
                    args_schema: json!({}),
                },
                ToolAction {
                    id: "admin.prompt_credential".to_owned(),
                    description: concat!(
                        "Get setup instructions for a service's API credential. ",
                        "For known services (notion, github, slack), instructions ",
                        "are provided automatically. Use this FIRST when setting up ",
                        "a new integration — the owner needs to provide the credential ",
                        "before the service can be connected."
                    )
                    .to_owned(),
                    semantics: ActionSemantics::Read,
                    label_ceiling: SecurityLabel::Sensitive,
                    args_schema: json!({
                        "service": "string (e.g. 'notion', 'github', 'slack')"
                    }),
                },
                ToolAction {
                    id: "admin.store_credential".to_owned(),
                    description: concat!(
                        "Store a credential (API token) in the secure vault. ",
                        "Use after the owner provides a credential value. ",
                        "The ref_id should match the one returned by admin.prompt_credential."
                    )
                    .to_owned(),
                    semantics: ActionSemantics::Write,
                    label_ceiling: SecurityLabel::Sensitive,
                    args_schema: json!({"ref_id": "string", "value": "string"}),
                },
                ToolAction {
                    id: "admin.list_mcp_servers".to_owned(),
                    description: "List currently connected external services".to_owned(),
                    semantics: ActionSemantics::Read,
                    label_ceiling: SecurityLabel::Sensitive,
                    args_schema: json!({}),
                },
                ToolAction {
                    id: "admin.connect_mcp_server".to_owned(),
                    description: concat!(
                        "Connect an external service (e.g. Notion, GitHub, Slack) ",
                        "and discover its tools. Known services: notion, github, slack, ",
                        "filesystem, fetch — just pass the name. ",
                        "IMPORTANT: credentials must be stored in vault first via ",
                        "admin.store_credential before connecting."
                    )
                    .to_owned(),
                    semantics: ActionSemantics::Write,
                    label_ceiling: SecurityLabel::Sensitive,
                    args_schema: json!({
                        "name": "string (service name, e.g. 'notion', 'github', 'slack')",
                        "command": "string (optional, for custom servers)",
                        "args": "string[] (optional, for custom servers)",
                        "label": "string (optional, security label, default: internal)",
                        "allowed_domains": "string[] (optional, network allowlist)"
                    }),
                },
                ToolAction {
                    id: "admin.disconnect_mcp_server".to_owned(),
                    description: "Disconnect an external service and remove all its tools"
                        .to_owned(),
                    semantics: ActionSemantics::Write,
                    label_ceiling: SecurityLabel::Sensitive,
                    args_schema: json!({"name": "string (service name, e.g. 'notion')"}),
                },
                ToolAction {
                    id: "admin.list_skills".to_owned(),
                    description: "List all locally deployed skills".to_owned(),
                    semantics: ActionSemantics::Read,
                    label_ceiling: SecurityLabel::Sensitive,
                    args_schema: json!({}),
                },
            ],
            network_allowlist: vec![], // Admin tool doesn't need network (spec 8.2).
        }
    }

    /// Execute an admin action (spec 8.2).
    async fn execute(
        &self,
        _cap: &ValidatedCapability,
        _creds: &InjectedCredentials,
        _http: &ScopedHttpClient,
        action: &str,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        match action {
            "admin.list_integrations" => self.list_integrations(),
            "admin.check_integration" => self.check_integration(args),
            "admin.list_templates" => self.list_templates(),
            "admin.system_status" => self.system_status(),
            "admin.prompt_credential" => self.prompt_credential(args),
            "admin.store_credential" => self.store_credential(args).await,
            "admin.list_mcp_servers" => self.list_mcp_servers().await,
            "admin.connect_mcp_server" => self.connect_mcp_server(args).await,
            "admin.disconnect_mcp_server" => self.disconnect_mcp_server(args).await,
            "admin.list_skills" => self.list_skills().await,
            _ => Err(ToolError::ActionNotFound(action.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::template::{InferenceConfig, TaskTemplate};
    use crate::kernel::vault::InMemoryVault;
    use crate::tools::calendar::CalendarTool;
    use crate::tools::email::EmailTool;
    use crate::types::{CapabilityToken, PrincipalClass, TaintLevel, TaintSet};
    use std::collections::HashSet;

    fn make_admin_tool() -> AdminTool {
        let vault: Arc<dyn SecretStore> = Arc::new(InMemoryVault::new());
        let tools = ToolRegistry::new();
        tools.register(Arc::new(EmailTool::new()));
        tools.register(Arc::new(CalendarTool::new()));
        let tools = Arc::new(tools);

        let mut templates = TemplateRegistry::new();
        templates.register(TaskTemplate {
            template_id: "owner_telegram_general".to_owned(),
            triggers: vec!["adapter:telegram:message:owner".to_owned()],
            principal_class: PrincipalClass::Owner,
            description: "General assistant".to_owned(),
            planner_task_description: None,
            allowed_tools: vec!["email.*".to_owned(), "admin.*".to_owned()],
            denied_tools: vec![],
            max_tool_calls: 15,
            max_tokens_plan: 4000,
            max_tokens_synthesize: 8000,
            output_sinks: vec!["sink:telegram:owner".to_owned()],
            data_ceiling: SecurityLabel::Sensitive,
            inference: InferenceConfig {
                provider: "local".to_owned(),
                model: "llama3".to_owned(),
                owner_acknowledged_cloud_risk: false,
            },
            require_approval_for_writes: false,
        });
        let templates = Arc::new(templates);

        AdminTool::new(vault, tools, templates)
    }

    fn make_cap(action: &str) -> ValidatedCapability {
        let token = CapabilityToken {
            capability_id: uuid::Uuid::new_v4(),
            task_id: uuid::Uuid::nil(),
            template_id: "test".to_owned(),
            principal: crate::types::Principal::Owner,
            tool: action.to_owned(),
            resource_scope: format!("tool:{action}"),
            taint_of_arguments: TaintSet {
                level: TaintLevel::Clean,
                origin: "owner".to_owned(),
                touched_by: vec![],
            },
            issued_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now(),
            max_invocations: 1,
        };
        ValidatedCapability::new(token)
    }

    fn make_http() -> ScopedHttpClient {
        ScopedHttpClient::new(HashSet::new())
    }

    // ── Manifest tests ──

    #[test]
    fn test_manifest_owner_only() {
        let admin = make_admin_tool();
        let manifest = admin.manifest();
        assert!(manifest.owner_only, "admin tool must be owner_only");
    }

    #[test]
    fn test_manifest_no_network() {
        let admin = make_admin_tool();
        let manifest = admin.manifest();
        assert!(
            manifest.network_allowlist.is_empty(),
            "admin tool should not need network access"
        );
    }

    #[test]
    fn test_manifest_label_ceilings() {
        let admin = make_admin_tool();
        let manifest = admin.manifest();
        for action in &manifest.actions {
            assert_eq!(
                action.label_ceiling,
                SecurityLabel::Sensitive,
                "admin action {} should have Sensitive ceiling (admin outputs never contain raw secrets)",
                action.id
            );
        }
    }

    // ── Action tests ──

    #[tokio::test]
    async fn test_list_integrations() {
        let admin = make_admin_tool();
        let cap = make_cap("admin.list_integrations");
        let creds = InjectedCredentials::new();
        let http = make_http();

        let result = admin
            .execute(&cap, &creds, &http, "admin.list_integrations", json!({}))
            .await;
        assert!(result.is_ok(), "list_integrations should succeed");

        let output = result.expect("checked");
        let tools = output.data["tools"].as_array().expect("should be array");
        assert!(
            tools.len() >= 2,
            "should list at least email and calendar tools"
        );
        assert!(!output.has_free_text);
    }

    #[tokio::test]
    async fn test_check_integration_exists() {
        let admin = make_admin_tool();
        let cap = make_cap("admin.check_integration");
        let creds = InjectedCredentials::new();
        let http = make_http();

        let result = admin
            .execute(
                &cap,
                &creds,
                &http,
                "admin.check_integration",
                json!({"service": "email"}),
            )
            .await;
        assert!(result.is_ok());

        let output = result.expect("checked");
        assert_eq!(output.data["exists"], json!(true));
        assert_eq!(output.data["service"], json!("email"));
        assert!(output.data["actions"].as_array().expect("array").len() >= 2);
    }

    #[tokio::test]
    async fn test_check_integration_not_found() {
        let admin = make_admin_tool();
        let cap = make_cap("admin.check_integration");
        let creds = InjectedCredentials::new();
        let http = make_http();

        let result = admin
            .execute(
                &cap,
                &creds,
                &http,
                "admin.check_integration",
                json!({"service": "notion"}),
            )
            .await;
        assert!(result.is_ok());

        let output = result.expect("checked");
        assert_eq!(output.data["exists"], json!(false));
    }

    #[tokio::test]
    async fn test_list_templates() {
        let admin = make_admin_tool();
        let cap = make_cap("admin.list_templates");
        let creds = InjectedCredentials::new();
        let http = make_http();

        let result = admin
            .execute(&cap, &creds, &http, "admin.list_templates", json!({}))
            .await;
        assert!(result.is_ok());

        let output = result.expect("checked");
        let templates = output.data["templates"]
            .as_array()
            .expect("should be array");
        assert_eq!(templates.len(), 1, "should have 1 template");
        assert_eq!(templates[0]["template_id"], json!("owner_telegram_general"));
    }

    #[tokio::test]
    async fn test_system_status() {
        let admin = make_admin_tool();
        let cap = make_cap("admin.system_status");
        let creds = InjectedCredentials::new();
        let http = make_http();

        let result = admin
            .execute(&cap, &creds, &http, "admin.system_status", json!({}))
            .await;
        assert!(result.is_ok());

        let output = result.expect("checked");
        // email has 2 actions, calendar has 1 = 3 total.
        assert!(output.data["tools_action_count"].as_u64().expect("u64") >= 3);
        assert_eq!(output.data["templates_count"], json!(1));
    }

    #[tokio::test]
    async fn test_prompt_credential_known_server() {
        let admin = make_admin_tool();
        let cap = make_cap("admin.prompt_credential");
        let creds = InjectedCredentials::new();
        let http = make_http();

        // For known servers (notion), instructions auto-fill from registry.
        let result = admin
            .execute(
                &cap,
                &creds,
                &http,
                "admin.prompt_credential",
                json!({"service": "notion"}),
            )
            .await;
        assert!(result.is_ok());

        let output = result.expect("checked");
        assert_eq!(output.data["prompt_required"], json!(true));
        assert_eq!(output.data["service"], json!("notion"));
        // Auto-filled from KnownServer registry.
        assert!(
            output.data["instructions"]
                .as_str()
                .expect("str")
                .contains("notion.so"),
            "known server should auto-fill setup instructions"
        );
        // Ref ID derived from known server env var name.
        assert!(
            output.data["ref_id"]
                .as_str()
                .expect("str")
                .starts_with("vault:notion_"),
            "ref_id should be vault-prefixed"
        );
    }

    #[tokio::test]
    async fn test_prompt_credential_unknown_server() {
        let admin = make_admin_tool();
        let cap = make_cap("admin.prompt_credential");
        let creds = InjectedCredentials::new();
        let http = make_http();

        // For unknown servers, uses args or defaults.
        let result = admin
            .execute(
                &cap,
                &creds,
                &http,
                "admin.prompt_credential",
                json!({
                    "service": "custom_api",
                    "credential_type": "bearer_token",
                    "instructions": "Get your token from the dashboard"
                }),
            )
            .await;
        assert!(result.is_ok());

        let output = result.expect("checked");
        assert_eq!(output.data["prompt_required"], json!(true));
        assert_eq!(output.data["service"], json!("custom_api"));
        assert_eq!(output.data["credential_type"], json!("bearer_token"));
        assert_eq!(
            output.data["ref_id"],
            json!("vault:custom_api_bearer_token")
        );
    }

    #[tokio::test]
    async fn test_prompt_credential_no_creds_needed() {
        let admin = make_admin_tool();
        let cap = make_cap("admin.prompt_credential");
        let creds = InjectedCredentials::new();
        let http = make_http();

        // filesystem is a known server with no credentials.
        let result = admin
            .execute(
                &cap,
                &creds,
                &http,
                "admin.prompt_credential",
                json!({"service": "filesystem"}),
            )
            .await;
        assert!(result.is_ok());

        let output = result.expect("checked");
        assert_eq!(
            output.data["prompt_required"],
            json!(false),
            "filesystem needs no credentials"
        );
    }

    #[tokio::test]
    async fn test_store_credential() {
        let vault = Arc::new(InMemoryVault::new());
        let tools = Arc::new(ToolRegistry::new());
        let templates = Arc::new(TemplateRegistry::new());
        let admin = AdminTool::new(Arc::clone(&vault) as Arc<dyn SecretStore>, tools, templates);

        let cap = make_cap("admin.store_credential");
        let creds = InjectedCredentials::new();
        let http = make_http();

        let result = admin
            .execute(
                &cap,
                &creds,
                &http,
                "admin.store_credential",
                json!({"ref_id": "vault:notion_token", "value": "ntn_secret_123"}),
            )
            .await;
        assert!(result.is_ok());

        let output = result.expect("checked");
        assert_eq!(output.data["stored"], json!(true));
        assert_eq!(output.data["ref_id"], json!("vault:notion_token"));
        // Value must NOT appear in output (invariant B).
        assert!(
            !output.data.to_string().contains("ntn_secret_123"),
            "secret value must not appear in tool output"
        );

        // Verify the secret was stored in vault.
        let stored = vault
            .get_secret("vault:notion_token")
            .await
            .expect("secret should be stored");
        assert_eq!(stored.expose(), "ntn_secret_123");
    }

    #[tokio::test]
    async fn test_unknown_action() {
        let admin = make_admin_tool();
        let cap = make_cap("admin.nonexistent");
        let creds = InjectedCredentials::new();
        let http = make_http();

        let result = admin
            .execute(&cap, &creds, &http, "admin.nonexistent", json!({}))
            .await;
        assert!(matches!(result, Err(ToolError::ActionNotFound(_))));
    }

    #[tokio::test]
    async fn test_check_integration_missing_arg() {
        let admin = make_admin_tool();
        let cap = make_cap("admin.check_integration");
        let creds = InjectedCredentials::new();
        let http = make_http();

        let result = admin
            .execute(&cap, &creds, &http, "admin.check_integration", json!({}))
            .await;
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }

    #[tokio::test]
    async fn test_list_skills_not_configured() {
        let admin = make_admin_tool(); // skills_dir is None
        let cap = make_cap("admin.list_skills");
        let creds = InjectedCredentials::new();
        let http = make_http();

        let result = admin
            .execute(&cap, &creds, &http, "admin.list_skills", json!({}))
            .await;
        assert!(
            matches!(result, Err(ToolError::ExecutionFailed(_))),
            "should fail when skills_dir is not configured"
        );
    }

    #[tokio::test]
    async fn test_list_skills_with_tempdir() {
        let tmp = tempfile::tempdir().expect("create tempdir");

        // Create a valid skill.
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir(&skill_dir).expect("mkdir");
        std::fs::write(
            skill_dir.join("skill.toml"),
            r#"
name = "my-skill"
description = "Test skill"
label = "public"

[server]
command = "python3"
"#,
        )
        .expect("write");

        let vault: Arc<dyn SecretStore> = Arc::new(InMemoryVault::new());
        let tools = Arc::new(ToolRegistry::new());
        let templates = Arc::new(TemplateRegistry::new());
        let admin = AdminTool::new(vault, tools, templates)
            .with_skills_dir(tmp.path().to_str().expect("path").to_owned());

        let cap = make_cap("admin.list_skills");
        let creds = InjectedCredentials::new();
        let http = make_http();

        let result = admin
            .execute(&cap, &creds, &http, "admin.list_skills", json!({}))
            .await;
        assert!(result.is_ok(), "list_skills should succeed");

        let output = result.expect("checked");
        assert_eq!(output.data["count"], json!(1));
        let skills = output.data["skills"].as_array().expect("array");
        assert_eq!(skills[0]["name"], json!("my-skill"));
        assert_eq!(skills[0]["status"], json!("on_disk"));
        assert!(!output.has_free_text);
    }

    #[tokio::test]
    async fn test_store_credential_missing_ref_id() {
        let admin = make_admin_tool();
        let cap = make_cap("admin.store_credential");
        let creds = InjectedCredentials::new();
        let http = make_http();

        let result = admin
            .execute(
                &cap,
                &creds,
                &http,
                "admin.store_credential",
                json!({"value": "secret"}),
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }
}
