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
use crate::tools::scoped_http::ScopedHttpClient;
use crate::tools::{
    ActionSemantics, InjectedCredentials, Tool, ToolAction, ToolError, ToolManifest, ToolOutput,
    ToolRegistry, ValidatedCapability,
};
use crate::types::SecurityLabel;

/// Admin tool for conversational configuration (spec 8.2).
///
/// Holds `Arc` references to kernel components, accessible only to
/// `principal:owner`. All actions carry `label_ceiling: Secret` since
/// admin operations may touch credential storage.
pub struct AdminTool {
    vault: Arc<dyn SecretStore>,
    tools: Arc<ToolRegistry>,
    templates: Arc<TemplateRegistry>,
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
        }
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
    /// This is a Read action that returns structured instructions. The
    /// synthesizer (Phase 3) turns this into a natural language message
    /// asking the owner to provide the credential.
    fn prompt_credential(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let service = args
            .get("service")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments("missing 'service' argument".to_owned()))?;

        let credential_type = args
            .get("credential_type")
            .and_then(|v| v.as_str())
            .unwrap_or("api_token");

        let instructions = args
            .get("instructions")
            .and_then(|v| v.as_str())
            .unwrap_or("Please provide the credential.");

        Ok(ToolOutput {
            data: json!({
                "prompt_required": true,
                "service": service,
                "credential_type": credential_type,
                "instructions": instructions,
                "ref_id": format!("vault:{service}_{credential_type}"),
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
                    description: "List all available and active integrations".to_owned(),
                    semantics: ActionSemantics::Read,
                    label_ceiling: SecurityLabel::Secret,
                    args_schema: json!({}),
                },
                ToolAction {
                    id: "admin.check_integration".to_owned(),
                    description: "Check if an integration module exists and its requirements"
                        .to_owned(),
                    semantics: ActionSemantics::Read,
                    label_ceiling: SecurityLabel::Secret,
                    args_schema: json!({"service": "string"}),
                },
                ToolAction {
                    id: "admin.list_templates".to_owned(),
                    description: "List all task templates".to_owned(),
                    semantics: ActionSemantics::Read,
                    label_ceiling: SecurityLabel::Secret,
                    args_schema: json!({}),
                },
                ToolAction {
                    id: "admin.system_status".to_owned(),
                    description: "Show active tools, templates, and system health".to_owned(),
                    semantics: ActionSemantics::Read,
                    label_ceiling: SecurityLabel::Secret,
                    args_schema: json!({}),
                },
                ToolAction {
                    id: "admin.prompt_credential".to_owned(),
                    description: "Ask owner for a credential and provide setup instructions"
                        .to_owned(),
                    semantics: ActionSemantics::Read,
                    label_ceiling: SecurityLabel::Secret,
                    args_schema: json!({
                        "service": "string",
                        "credential_type": "string (optional, default: api_token)",
                        "instructions": "string (optional)"
                    }),
                },
                ToolAction {
                    id: "admin.store_credential".to_owned(),
                    description: "Store a credential value in the vault".to_owned(),
                    semantics: ActionSemantics::Write,
                    label_ceiling: SecurityLabel::Secret,
                    args_schema: json!({"ref_id": "string", "value": "string"}),
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
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(EmailTool::new()));
        tools.register(Box::new(CalendarTool::new()));
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
    fn test_manifest_secret_ceiling() {
        let admin = make_admin_tool();
        let manifest = admin.manifest();
        for action in &manifest.actions {
            assert_eq!(
                action.label_ceiling,
                SecurityLabel::Secret,
                "admin action {} should have Secret ceiling",
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
    async fn test_prompt_credential() {
        let admin = make_admin_tool();
        let cap = make_cap("admin.prompt_credential");
        let creds = InjectedCredentials::new();
        let http = make_http();

        let result = admin
            .execute(
                &cap,
                &creds,
                &http,
                "admin.prompt_credential",
                json!({
                    "service": "notion",
                    "credential_type": "integration_token",
                    "instructions": "Go to notion.so/my-integrations"
                }),
            )
            .await;
        assert!(result.is_ok());

        let output = result.expect("checked");
        assert_eq!(output.data["prompt_required"], json!(true));
        assert_eq!(output.data["service"], json!("notion"));
        assert_eq!(output.data["credential_type"], json!("integration_token"));
        assert!(output.data["instructions"]
            .as_str()
            .expect("str")
            .contains("notion.so"));
        assert_eq!(
            output.data["ref_id"],
            json!("vault:notion_integration_token")
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
