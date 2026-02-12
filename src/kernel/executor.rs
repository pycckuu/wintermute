//! Plan executor — Phase 2 of the Plan-Then-Execute pipeline (spec 7).
//!
//! Mechanically executes plan steps with no LLM involvement.
//! Each step is validated against the policy engine before execution.
//! The kernel issues capability tokens, resolves credentials, creates
//! scoped HTTP clients, and dispatches to tool modules in-process.

use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn};

use crate::kernel::audit::AuditLogger;
use crate::kernel::policy::{PolicyEngine, PolicyError};
use crate::kernel::vault::SecretStore;
use crate::tools::scoped_http::ScopedHttpClient;
use crate::tools::{
    ActionSemantics, InjectedCredentials, ToolOutput, ToolRegistry, ValidatedCapability,
};
use crate::types::{ApprovalDecision, SecurityLabel, TaintSet, Task};

/// A single step in an execution plan (spec 10.4).
///
/// Shared with planner module — will be unified in the pipeline module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    /// Step ordinal (1-based).
    pub step: usize,
    /// Fully qualified tool action ID (e.g. "email.list").
    pub tool: String,
    /// Arguments to pass to the tool action.
    pub args: serde_json::Value,
}

/// Result of executing a single plan step (spec 10.6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecutionResult {
    /// Step ordinal matching the plan step.
    pub step: usize,
    /// Tool action that was executed.
    pub tool: String,
    /// Structured output from the tool.
    pub output: serde_json::Value,
    /// Security label assigned by kernel after applying ceiling.
    pub label: SecurityLabel,
    /// Whether the tool execution succeeded.
    pub success: bool,
    /// Error message if execution failed.
    pub error: Option<String>,
}

/// Executor errors (spec 7 Phase 2).
#[derive(Debug, Error)]
pub enum ExecutorError {
    /// Policy engine rejected the tool invocation.
    #[error("policy error: {0}")]
    Policy(#[from] PolicyError),
    /// Tool execution returned an error.
    #[error("tool error: {0}")]
    Tool(String),
    /// Requested tool or action not found in registry.
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    /// Plan exceeds the task template's max_tool_calls limit.
    #[error("max tool calls exceeded: limit {limit}")]
    MaxToolCallsExceeded {
        /// The template-defined maximum.
        limit: u32,
    },
    /// A write operation requires human approval before proceeding.
    #[error("approval required: {0}")]
    ApprovalRequired(String),
    /// Vault error during credential resolution.
    #[error("vault error: {0}")]
    VaultError(String),
}

/// Plan executor dispatching tool calls with policy enforcement (spec 7).
///
/// Receives an ordered plan from the Planner and executes each step
/// mechanically. No LLM is involved. Every invocation is validated
/// against the task template, taint rules, and label ceilings.
pub struct PlanExecutor {
    policy: Arc<PolicyEngine>,
    tools: Arc<ToolRegistry>,
    #[allow(dead_code)] // Used for credential resolution in later phases.
    vault: Arc<dyn SecretStore>,
    audit: Arc<AuditLogger>,
}

impl PlanExecutor {
    /// Create a new plan executor (spec 7 Phase 2).
    pub fn new(
        policy: Arc<PolicyEngine>,
        tools: Arc<ToolRegistry>,
        vault: Arc<dyn SecretStore>,
        audit: Arc<AuditLogger>,
    ) -> Self {
        Self {
            policy,
            tools,
            vault,
            audit,
        }
    }

    /// Execute a plan step-by-step (spec 7, Phase 2).
    ///
    /// For each step:
    /// 1. Check against max_tool_calls limit
    /// 2. Look up tool and action in registry
    /// 3. Check if action is a write; if so, apply graduated taint rules
    /// 4. Issue capability token via policy engine
    /// 5. Resolve credentials from vault (empty in Phase 2 test mode)
    /// 6. Create ScopedHttpClient from tool manifest's network allowlist
    /// 7. Call tool.execute()
    /// 8. Apply label ceiling to result
    /// 9. Audit log the invocation
    ///
    /// Returns all step results on success, or the first error encountered.
    pub async fn execute_plan(
        &self,
        task: &Task,
        steps: &[PlanStep],
        event_taint: &TaintSet,
    ) -> Result<Vec<StepExecutionResult>, ExecutorError> {
        let mut results = Vec::new();

        for (i, step) in steps.iter().enumerate() {
            // Step 1: Check max tool calls.
            let call_count = u32::try_from(i.saturating_add(1)).unwrap_or(u32::MAX);
            if call_count > task.max_tool_calls {
                return Err(ExecutorError::MaxToolCallsExceeded {
                    limit: task.max_tool_calls,
                });
            }

            info!(step = step.step, tool = %step.tool, "executing plan step");

            // Step 2: Look up tool and action in registry.
            let (tool, action) = self
                .tools
                .get_tool_and_action(&step.tool)
                .ok_or_else(|| ExecutorError::ToolNotFound(step.tool.clone()))?;

            // Step 3: Check taint for write operations (spec 4.4).
            if action.semantics == ActionSemantics::Write {
                // Conservative: assume free text for writes.
                let has_free_text = true;
                let decision = self.policy.check_taint(event_taint, has_free_text);
                if let ApprovalDecision::RequiresHumanApproval { reason } = decision {
                    warn!(tool = %step.tool, %reason, "write requires approval");
                    return Err(ExecutorError::ApprovalRequired(reason));
                }
            }

            // Step 4: Issue capability token (validates against template).
            let cap_token = self.policy.issue_capability(
                task,
                &step.tool,
                format!("tool:{}", step.tool),
                event_taint.clone(),
            )?;

            // Step 5: Create validated capability.
            let validated_cap = ValidatedCapability::new(cap_token.clone());

            // Step 6: Resolve credentials (empty for Phase 2 test mode).
            // Production will look up tool manifest's credential_ref in vault.
            let creds = InjectedCredentials::new();

            // Step 7: Create ScopedHttpClient from tool manifest's network allowlist.
            let manifest = tool.manifest();
            let allowed_domains: HashSet<String> = manifest.network_allowlist.into_iter().collect();
            let http = ScopedHttpClient::new(allowed_domains);

            // Step 8: Execute tool.
            let tool_output: ToolOutput = tool
                .execute(&validated_cap, &creds, &http, &step.tool, step.args.clone())
                .await
                .map_err(|e| ExecutorError::Tool(e.to_string()))?;

            // Step 9: Apply label ceiling to result (spec 6.2).
            let final_label = self
                .policy
                .apply_label_ceiling(&step.tool, action.label_ceiling);

            // Step 10: Audit log the invocation (spec 6.7).
            // Best-effort: don't fail execution if audit write fails.
            if let Err(e) = self.audit.log_tool_invoked(&cap_token, &step.args) {
                warn!(error = %e, "failed to audit log tool invocation");
            }

            results.push(StepExecutionResult {
                step: step.step,
                tool: step.tool.clone(),
                output: tool_output.data,
                label: final_label,
                success: true,
                error: None,
            });
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::audit::AuditLogger;
    use crate::kernel::policy::PolicyEngine;
    use crate::kernel::vault::InMemoryVault;
    use crate::tools::scoped_http::ScopedHttpClient;
    use crate::tools::{Tool, ToolAction, ToolError, ToolManifest, ToolOutput};
    use crate::types::{Principal, TaintLevel, TaintSet, TaskState};
    use async_trait::async_trait;
    use std::io::{Cursor, Write};
    use std::sync::Mutex;
    use uuid::Uuid;

    // ── Test helpers ──

    /// Shared buffer for capturing audit output in tests.
    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Cursor<Vec<u8>>>>);

    impl SharedBuf {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Cursor::new(Vec::new()))))
        }

        fn contents(&self) -> String {
            let cursor = self.0.lock().expect("test lock");
            String::from_utf8_lossy(cursor.get_ref()).to_string()
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

    /// Mock tool for executor tests.
    struct MockTool {
        name: String,
        actions: Vec<ToolAction>,
    }

    impl MockTool {
        fn read_tool() -> Self {
            Self {
                name: "email".to_owned(),
                actions: vec![
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
            }
        }

        fn write_tool() -> Self {
            Self {
                name: "email".to_owned(),
                actions: vec![
                    ToolAction {
                        id: "email.list".to_owned(),
                        description: "List recent emails".to_owned(),
                        semantics: ActionSemantics::Read,
                        label_ceiling: SecurityLabel::Sensitive,
                        args_schema: serde_json::json!({"account": "string"}),
                    },
                    ToolAction {
                        id: "email.send".to_owned(),
                        description: "Send an email".to_owned(),
                        semantics: ActionSemantics::Write,
                        label_ceiling: SecurityLabel::Sensitive,
                        args_schema: serde_json::json!({"to": "string", "body": "string"}),
                    },
                ],
            }
        }

        fn calendar_tool() -> Self {
            Self {
                name: "calendar".to_owned(),
                actions: vec![ToolAction {
                    id: "calendar.freebusy".to_owned(),
                    description: "Get free/busy status".to_owned(),
                    semantics: ActionSemantics::Read,
                    label_ceiling: SecurityLabel::Sensitive,
                    args_schema: serde_json::json!({"date": "string"}),
                }],
            }
        }
    }

    #[async_trait]
    impl Tool for MockTool {
        fn manifest(&self) -> ToolManifest {
            ToolManifest {
                name: self.name.clone(),
                owner_only: false,
                actions: self.actions.clone(),
                network_allowlist: vec!["api.example.com".to_owned()],
            }
        }

        async fn execute(
            &self,
            _cap: &ValidatedCapability,
            _creds: &InjectedCredentials,
            _http: &ScopedHttpClient,
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

    fn test_task() -> Task {
        Task {
            task_id: Uuid::nil(),
            template_id: "test_template".to_owned(),
            principal: Principal::Owner,
            trigger_event: Uuid::nil(),
            data_ceiling: SecurityLabel::Sensitive,
            allowed_tools: vec![
                "email.list".to_owned(),
                "email.read".to_owned(),
                "email.send".to_owned(),
                "calendar.freebusy".to_owned(),
            ],
            denied_tools: vec![],
            max_tool_calls: 10,
            output_sinks: vec!["sink:telegram:owner".to_owned()],
            trace_id: "test-trace".to_owned(),
            state: TaskState::Executing { current_step: 0 },
        }
    }

    fn clean_taint() -> TaintSet {
        TaintSet {
            level: TaintLevel::Clean,
            origin: "owner".to_owned(),
            touched_by: vec![],
        }
    }

    fn raw_taint() -> TaintSet {
        TaintSet {
            level: TaintLevel::Raw,
            origin: "external".to_owned(),
            touched_by: vec![],
        }
    }

    fn make_executor(buf: &SharedBuf) -> PlanExecutor {
        let policy = Arc::new(PolicyEngine::with_defaults());
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockTool::read_tool()));
        registry.register(Box::new(MockTool::calendar_tool()));
        let tools = Arc::new(registry);
        let vault: Arc<dyn SecretStore> = Arc::new(InMemoryVault::new());
        let audit = Arc::new(AuditLogger::from_writer(Box::new(buf.clone())));

        PlanExecutor::new(policy, tools, vault, audit)
    }

    fn make_executor_with_write_tool(buf: &SharedBuf) -> PlanExecutor {
        let policy = Arc::new(PolicyEngine::with_defaults());
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockTool::write_tool()));
        registry.register(Box::new(MockTool::calendar_tool()));
        let tools = Arc::new(registry);
        let vault: Arc<dyn SecretStore> = Arc::new(InMemoryVault::new());
        let audit = Arc::new(AuditLogger::from_writer(Box::new(buf.clone())));

        PlanExecutor::new(policy, tools, vault, audit)
    }

    // ── Tests ──

    #[tokio::test]
    async fn test_execute_single_read_step() {
        let buf = SharedBuf::new();
        let executor = make_executor(&buf);
        let task = test_task();
        let steps = vec![PlanStep {
            step: 1,
            tool: "email.list".to_owned(),
            args: serde_json::json!({"account": "personal"}),
        }];

        let results = executor
            .execute_plan(&task, &steps, &clean_taint())
            .await
            .expect("should succeed");

        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        assert_eq!(results[0].tool, "email.list");
        assert_eq!(results[0].output["status"], "ok");
        // email.list has a ceiling of Sensitive in default policy.
        assert_eq!(results[0].label, SecurityLabel::Sensitive);
    }

    #[tokio::test]
    async fn test_execute_multiple_steps() {
        let buf = SharedBuf::new();
        let executor = make_executor(&buf);
        let task = test_task();
        let steps = vec![
            PlanStep {
                step: 1,
                tool: "email.list".to_owned(),
                args: serde_json::json!({"account": "personal"}),
            },
            PlanStep {
                step: 2,
                tool: "email.read".to_owned(),
                args: serde_json::json!({"message_id": "msg_123"}),
            },
        ];

        let results = executor
            .execute_plan(&task, &steps, &clean_taint())
            .await
            .expect("should succeed");

        assert_eq!(results.len(), 2);
        assert!(results[0].success);
        assert!(results[1].success);
        assert_eq!(results[0].tool, "email.list");
        assert_eq!(results[1].tool, "email.read");
    }

    #[tokio::test]
    async fn test_execute_write_clean_taint() {
        let buf = SharedBuf::new();
        let executor = make_executor_with_write_tool(&buf);
        let task = test_task();
        let steps = vec![PlanStep {
            step: 1,
            tool: "email.send".to_owned(),
            args: serde_json::json!({"to": "alice@co", "body": "hello"}),
        }];

        // Clean taint -> write should be auto-approved.
        let results = executor
            .execute_plan(&task, &steps, &clean_taint())
            .await
            .expect("clean taint write should succeed");

        assert_eq!(results.len(), 1);
        assert!(results[0].success);
    }

    #[tokio::test]
    async fn test_execute_write_raw_taint() {
        let buf = SharedBuf::new();
        let executor = make_executor_with_write_tool(&buf);
        let task = test_task();
        let steps = vec![PlanStep {
            step: 1,
            tool: "email.send".to_owned(),
            args: serde_json::json!({"to": "alice@co", "body": "injected"}),
        }];

        // Raw taint -> write should require approval.
        let result = executor.execute_plan(&task, &steps, &raw_taint()).await;

        assert!(result.is_err());
        let err = result.expect_err("should be approval required");
        assert!(
            matches!(err, ExecutorError::ApprovalRequired(_)),
            "expected ApprovalRequired, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_execute_tool_not_found() {
        let buf = SharedBuf::new();
        let executor = make_executor(&buf);
        let task = test_task();
        let steps = vec![PlanStep {
            step: 1,
            tool: "nonexistent.action".to_owned(),
            args: serde_json::json!({}),
        }];

        let result = executor.execute_plan(&task, &steps, &clean_taint()).await;

        assert!(result.is_err());
        let err = result.expect_err("should be tool not found");
        assert!(
            matches!(err, ExecutorError::ToolNotFound(ref t) if t == "nonexistent.action"),
            "expected ToolNotFound, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_execute_max_calls_exceeded() {
        let buf = SharedBuf::new();
        let executor = make_executor(&buf);
        let mut task = test_task();
        task.max_tool_calls = 2;

        let steps = vec![
            PlanStep {
                step: 1,
                tool: "email.list".to_owned(),
                args: serde_json::json!({}),
            },
            PlanStep {
                step: 2,
                tool: "email.read".to_owned(),
                args: serde_json::json!({}),
            },
            PlanStep {
                step: 3,
                tool: "email.list".to_owned(),
                args: serde_json::json!({}),
            },
        ];

        let result = executor.execute_plan(&task, &steps, &clean_taint()).await;

        assert!(result.is_err());
        let err = result.expect_err("should exceed max calls");
        assert!(
            matches!(err, ExecutorError::MaxToolCallsExceeded { limit: 2 }),
            "expected MaxToolCallsExceeded with limit 2, got: {err}"
        );
    }

    /// Regression test 5: Tool's reported label overridden by kernel ceiling.
    ///
    /// calendar.freebusy has a kernel ceiling of `Internal` (spec 4.3).
    /// Even if the tool action declares `Sensitive`, the kernel applies
    /// its authoritative ceiling.
    #[tokio::test]
    async fn test_execute_label_ceiling_applied() {
        let buf = SharedBuf::new();
        let executor = make_executor(&buf);
        let task = test_task();
        let steps = vec![PlanStep {
            step: 1,
            tool: "calendar.freebusy".to_owned(),
            args: serde_json::json!({"date": "2026-02-12"}),
        }];

        let results = executor
            .execute_plan(&task, &steps, &clean_taint())
            .await
            .expect("should succeed");

        assert_eq!(results.len(), 1);
        // The mock tool declares Sensitive for calendar.freebusy, but
        // PolicyEngine::with_defaults() sets a ceiling of Internal.
        assert_eq!(
            results[0].label,
            SecurityLabel::Internal,
            "kernel ceiling should override tool's reported label"
        );
    }

    /// Regression test 8: Tool in denied_tools returns PolicyError.
    ///
    /// When the planner requests a tool that is in the template's
    /// denied_tools list, the policy engine must reject it.
    #[tokio::test]
    async fn test_execute_denied_tool() {
        let buf = SharedBuf::new();
        let executor = make_executor(&buf);
        let mut task = test_task();
        // Deny email.list explicitly.
        task.denied_tools = vec!["email.list".to_owned()];

        let steps = vec![PlanStep {
            step: 1,
            tool: "email.list".to_owned(),
            args: serde_json::json!({}),
        }];

        let result = executor.execute_plan(&task, &steps, &clean_taint()).await;

        assert!(result.is_err());
        let err = result.expect_err("should be policy error");
        assert!(
            matches!(err, ExecutorError::Policy(PolicyError::ToolDenied { .. })),
            "expected ToolDenied policy error, got: {err}"
        );
    }

    /// Verify that the audit logger is called on successful tool execution.
    #[tokio::test]
    async fn test_audit_logged_on_execution() {
        let buf = SharedBuf::new();
        let executor = make_executor(&buf);
        let task = test_task();
        let steps = vec![PlanStep {
            step: 1,
            tool: "email.list".to_owned(),
            args: serde_json::json!({"account": "personal"}),
        }];

        let _results = executor
            .execute_plan(&task, &steps, &clean_taint())
            .await
            .expect("should succeed");

        let audit_output = buf.contents();
        assert!(
            !audit_output.is_empty(),
            "audit log should have entries after execution"
        );
        assert!(
            audit_output.contains("tool_invoked"),
            "audit log should contain tool_invoked event"
        );
        assert!(
            audit_output.contains("email.list"),
            "audit log should contain the tool name"
        );
    }

    /// Verify that a tool not in allowed_tools is rejected by policy.
    #[tokio::test]
    async fn test_execute_tool_not_allowed_by_template() {
        let buf = SharedBuf::new();
        let executor = make_executor(&buf);
        let mut task = test_task();
        // Restrict allowed tools to only calendar.
        task.allowed_tools = vec!["calendar.freebusy".to_owned()];

        let steps = vec![PlanStep {
            step: 1,
            tool: "email.list".to_owned(),
            args: serde_json::json!({}),
        }];

        let result = executor.execute_plan(&task, &steps, &clean_taint()).await;

        assert!(result.is_err());
        let err = result.expect_err("should be policy error");
        assert!(
            matches!(
                err,
                ExecutorError::Policy(PolicyError::ToolNotAllowed { .. })
            ),
            "expected ToolNotAllowed policy error, got: {err}"
        );
    }
}
