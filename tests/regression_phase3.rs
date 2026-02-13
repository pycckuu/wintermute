#![allow(missing_docs)]
//! Phase 3 regression tests (spec section 17).
//!
//! Test 15: admin.* tools reject invocation from any principal except owner.
//! Validates conversational configuration security (spec 8.2).

use std::io::{Cursor, Write};
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use pfar::kernel::audit::AuditLogger;
use pfar::kernel::executor::{PlanExecutor, PlanStep};
use pfar::kernel::policy::PolicyEngine;
use pfar::kernel::template::TemplateRegistry;
use pfar::kernel::vault::InMemoryVault;
use pfar::tools::admin::AdminTool;
use pfar::tools::ToolRegistry;
use pfar::types::{Principal, SecurityLabel, TaintLevel, TaintSet, Task, TaskState};

// ── Test infrastructure ──

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

fn clean_taint() -> TaintSet {
    TaintSet {
        level: TaintLevel::Clean,
        origin: "owner".to_owned(),
        touched_by: vec![],
    }
}

// =========================================================================
// Regression Test 15: Admin Tools Reject Non-Owner (spec 8.2, 17.15)
// =========================================================================

/// admin.* tools reject invocation from any principal except owner.
///
/// End-to-end executor test: registers the real AdminTool, creates a
/// task with a non-owner principal, and verifies that the executor's
/// owner_only check rejects the invocation before the tool runs.
#[tokio::test]
async fn regression_15_admin_tools_reject_non_owner() {
    let buf = SharedBuf::new();
    let policy = Arc::new(PolicyEngine::with_defaults());
    let audit = Arc::new(AuditLogger::from_writer(Box::new(buf)));

    // Build tool registry with the real AdminTool.
    let vault: Arc<dyn pfar::kernel::vault::SecretStore> = Arc::new(InMemoryVault::new());
    let templates = Arc::new(TemplateRegistry::new());
    let base_tools = ToolRegistry::new();
    // No base tools needed — we just need the admin tool.
    let base_tools_arc = Arc::new(base_tools);
    let admin = AdminTool::new(
        Arc::clone(&vault),
        Arc::clone(&base_tools_arc),
        Arc::clone(&templates),
    );

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(admin));
    let tools = Arc::new(registry);

    let executor = PlanExecutor::new(
        Arc::clone(&policy),
        Arc::clone(&tools),
        Arc::clone(&vault),
        Arc::clone(&audit),
    );

    // Task with a third-party principal (not owner).
    let task = Task {
        task_id: Uuid::nil(),
        template_id: "whatsapp_scheduling".to_owned(),
        principal: Principal::WhatsAppContact("+34665030077".to_owned()),
        trigger_event: Uuid::nil(),
        data_ceiling: SecurityLabel::Internal,
        allowed_tools: vec!["admin.*".to_owned()],
        denied_tools: vec![],
        max_tool_calls: 5,
        output_sinks: vec!["sink:whatsapp:reply_to_sender".to_owned()],
        trace_id: "regression-15".to_owned(),
        state: TaskState::Executing { current_step: 0 },
    };

    // Try admin.system_status — should be rejected as OwnerOnly.
    let steps = vec![PlanStep {
        step: 1,
        tool: "admin.system_status".to_owned(),
        args: serde_json::json!({}),
    }];

    let result = executor.execute_plan(&task, &steps, &clean_taint()).await;
    assert!(
        result.is_err(),
        "admin tool invoked by non-owner should fail"
    );

    let err = result.expect_err("should be OwnerOnly error");
    let err_str = format!("{err}");
    assert!(
        err_str.contains("owner-only"),
        "error should mention owner-only restriction: {err_str}"
    );
}

/// admin.* tools succeed when invoked by owner principal.
#[tokio::test]
async fn regression_15_admin_tools_allow_owner() {
    let buf = SharedBuf::new();
    let policy = Arc::new(PolicyEngine::with_defaults());
    let audit = Arc::new(AuditLogger::from_writer(Box::new(buf)));

    let vault: Arc<dyn pfar::kernel::vault::SecretStore> = Arc::new(InMemoryVault::new());
    let templates = Arc::new(TemplateRegistry::new());
    let base_tools_arc = Arc::new(ToolRegistry::new());
    let admin = AdminTool::new(
        Arc::clone(&vault),
        Arc::clone(&base_tools_arc),
        Arc::clone(&templates),
    );

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(admin));
    let tools = Arc::new(registry);

    let executor = PlanExecutor::new(
        Arc::clone(&policy),
        Arc::clone(&tools),
        Arc::clone(&vault),
        Arc::clone(&audit),
    );

    // Task with owner principal.
    let task = Task {
        task_id: Uuid::nil(),
        template_id: "owner_telegram_general".to_owned(),
        principal: Principal::Owner,
        trigger_event: Uuid::nil(),
        data_ceiling: SecurityLabel::Sensitive,
        allowed_tools: vec!["admin.*".to_owned()],
        denied_tools: vec![],
        max_tool_calls: 15,
        output_sinks: vec!["sink:telegram:owner".to_owned()],
        trace_id: "regression-15-owner".to_owned(),
        state: TaskState::Executing { current_step: 0 },
    };

    let steps = vec![PlanStep {
        step: 1,
        tool: "admin.system_status".to_owned(),
        args: serde_json::json!({}),
    }];

    let results = executor
        .execute_plan(&task, &steps, &clean_taint())
        .await
        .expect("owner should be allowed to invoke admin tools");

    assert_eq!(results.len(), 1);
    assert!(results[0].success);
}

/// admin.store_credential invoked by Telegram peer should be rejected.
#[tokio::test]
async fn regression_15_admin_store_credential_rejects_peer() {
    let buf = SharedBuf::new();
    let policy = Arc::new(PolicyEngine::with_defaults());
    let audit = Arc::new(AuditLogger::from_writer(Box::new(buf)));

    let vault: Arc<dyn pfar::kernel::vault::SecretStore> = Arc::new(InMemoryVault::new());
    let templates = Arc::new(TemplateRegistry::new());
    let base_tools_arc = Arc::new(ToolRegistry::new());
    let admin = AdminTool::new(
        Arc::clone(&vault),
        Arc::clone(&base_tools_arc),
        Arc::clone(&templates),
    );

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(admin));
    let tools = Arc::new(registry);

    let executor = PlanExecutor::new(
        Arc::clone(&policy),
        Arc::clone(&tools),
        Arc::clone(&vault),
        Arc::clone(&audit),
    );

    // Telegram peer trying to store a credential.
    let task = Task {
        task_id: Uuid::nil(),
        template_id: "test".to_owned(),
        principal: Principal::TelegramPeer("attacker_123".to_owned()),
        trigger_event: Uuid::nil(),
        data_ceiling: SecurityLabel::Sensitive,
        allowed_tools: vec!["admin.*".to_owned()],
        denied_tools: vec![],
        max_tool_calls: 5,
        output_sinks: vec!["sink:telegram:owner".to_owned()],
        trace_id: "regression-15-cred".to_owned(),
        state: TaskState::Executing { current_step: 0 },
    };

    let steps = vec![PlanStep {
        step: 1,
        tool: "admin.store_credential".to_owned(),
        args: serde_json::json!({"ref_id": "vault:stolen", "value": "malicious"}),
    }];

    let result = executor.execute_plan(&task, &steps, &clean_taint()).await;
    assert!(
        result.is_err(),
        "non-owner should not be able to store credentials"
    );

    // Verify nothing was stored in vault.
    let check = vault.get_secret("vault:stolen").await;
    assert!(
        check.is_err(),
        "vault should not contain the credential after rejection"
    );
}
