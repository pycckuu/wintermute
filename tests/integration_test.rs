#![allow(missing_docs)]
// Integration tests for Phase 1 kernel core.
//
// Tests the end-to-end flow: CLI event → router → template match →
// task creation with correct policy enforcement.
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use pfar::adapters::cli::create_cli_event;
use pfar::kernel::audit::AuditLogger;
use pfar::kernel::policy::PolicyEngine;
use pfar::kernel::router::EventRouter;
use pfar::kernel::template::TemplateRegistry;
use pfar::types::{
    EventKind, EventPayload, EventSource, InboundEvent, Principal, SecurityLabel, TaintLevel,
};

// ── Test fixtures ──

const OWNER_TEMPLATE: &str = r#"
template_id = "owner_cli_general"
triggers = ["adapter:cli:message:owner"]
principal_class = "owner"
description = "General assistant for owner via CLI"
allowed_tools = ["email.list", "email.read", "calendar.freebusy"]
max_tool_calls = 15
output_sinks = ["sink:telegram:owner"]
data_ceiling = "sensitive"

[inference]
provider = "local"
model = "llama3"
"#;

const SCHEDULING_TEMPLATE: &str = r#"
template_id = "whatsapp_scheduling"
triggers = ["adapter:whatsapp:message:third_party"]
principal_class = "third_party"
description = "Handle scheduling requests from WhatsApp contacts"
planner_task_description = "A contact is requesting to schedule a meeting."
allowed_tools = ["calendar.freebusy", "message.reply"]
denied_tools = ["email.send"]
max_tool_calls = 5
output_sinks = ["sink:whatsapp:reply_to_sender"]
data_ceiling = "internal"

[inference]
provider = "local"
model = "llama3"
"#;

/// Shared buffer for capturing audit output.
#[derive(Clone)]
struct AuditBuf(Arc<Mutex<Cursor<Vec<u8>>>>);

impl AuditBuf {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Cursor::new(Vec::new()))))
    }
    fn contents(&self) -> String {
        let c = self.0.lock().expect("test lock");
        String::from_utf8_lossy(c.get_ref()).to_string()
    }
}

impl std::io::Write for AuditBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("test lock").write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().expect("test lock").flush()
    }
}

fn build_router(audit_buf: &AuditBuf) -> EventRouter {
    let policy = Arc::new(PolicyEngine::with_defaults());
    let audit = Arc::new(AuditLogger::from_writer(Box::new(audit_buf.clone())));

    let mut templates = TemplateRegistry::new();
    templates.register(toml::from_str(OWNER_TEMPLATE).expect("parse owner template"));
    templates.register(toml::from_str(SCHEDULING_TEMPLATE).expect("parse scheduling template"));

    EventRouter::new(policy, Arc::new(templates), audit)
}

// ── Integration tests ──

/// Task 1.12: CLI event → template match → task creation → audit log.
#[test]
fn test_cli_event_to_task() {
    let buf = AuditBuf::new();
    let router = build_router(&buf);

    // CLI event always produces Owner principal.
    let event = create_cli_event("Check my email");
    let (labeled, task) = router.route_event(event).expect("should route");

    // Verify task matches owner template.
    assert_eq!(task.template_id, "owner_cli_general");
    assert_eq!(task.principal, Principal::Owner);
    assert_eq!(task.data_ceiling, SecurityLabel::Sensitive);
    assert!(task.allowed_tools.contains(&"email.list".to_owned()));
    assert!(task.allowed_tools.contains(&"email.read".to_owned()));
    assert_eq!(task.output_sinks, vec!["sink:telegram:owner"]);

    // Verify label and taint assignment.
    assert_eq!(labeled.label, SecurityLabel::Sensitive);
    assert_eq!(labeled.taint.level, TaintLevel::Clean);

    // Verify audit log was written.
    let audit_output = buf.contents();
    assert!(!audit_output.is_empty());
    let entry: serde_json::Value =
        serde_json::from_str(audit_output.trim()).expect("valid JSON audit entry");
    assert_eq!(entry["event_type"], "task_created");
    assert_eq!(entry["details"]["template_id"], "owner_cli_general");
}

/// Third-party event from WhatsApp → scheduling template.
#[test]
fn test_third_party_whatsapp_routing() {
    let buf = AuditBuf::new();
    let router = build_router(&buf);

    let event = InboundEvent {
        event_id: uuid::Uuid::new_v4(),
        timestamp: chrono::Utc::now(),
        source: EventSource {
            adapter: "whatsapp".to_owned(),
            principal: Principal::WhatsAppContact("+34665030077".to_owned()),
        },
        kind: EventKind::Message,
        payload: EventPayload {
            text: Some("Can we meet Tuesday?".to_owned()),
            attachments: vec![],
            reply_to: None,
            metadata: serde_json::json!({}),
        },
    };

    let (labeled, task) = router.route_event(event).expect("should route");

    assert_eq!(task.template_id, "whatsapp_scheduling");
    assert_eq!(task.data_ceiling, SecurityLabel::Internal);
    assert!(task.allowed_tools.contains(&"calendar.freebusy".to_owned()));
    assert!(!task.allowed_tools.contains(&"email.list".to_owned()));
    assert_eq!(labeled.label, SecurityLabel::Internal);
    assert_eq!(labeled.taint.level, TaintLevel::Raw);
    assert!(labeled.taint.origin.contains("+34665030077"));
}

/// No template match → error.
#[test]
fn test_no_template_match() {
    let buf = AuditBuf::new();
    let router = build_router(&buf);

    let event = InboundEvent {
        event_id: uuid::Uuid::new_v4(),
        timestamp: chrono::Utc::now(),
        source: EventSource {
            adapter: "slack".to_owned(),
            principal: Principal::SlackUser {
                workspace: "w1".to_owned(),
                channel: "c1".to_owned(),
                user: "u1".to_owned(),
            },
        },
        kind: EventKind::Message,
        payload: EventPayload {
            text: Some("hello".to_owned()),
            attachments: vec![],
            reply_to: None,
            metadata: serde_json::json!({}),
        },
    };

    let result = router.route_event(event);
    assert!(result.is_err());
}

/// Policy enforcement: No Write Down — sensitive data cannot flow to public sink.
#[test]
fn test_no_write_down_enforcement() {
    let engine = PolicyEngine::with_defaults();

    // Sensitive data → public sink (WhatsApp third party) → DENIED.
    let result = engine.check_write(SecurityLabel::Sensitive, SecurityLabel::Public);
    assert!(result.is_err());

    // Regulated data → public sink → DENIED.
    let result = engine.check_write(SecurityLabel::Regulated, SecurityLabel::Public);
    assert!(result.is_err());

    // Internal data → sensitive sink → ALLOWED (write up is OK).
    let result = engine.check_write(SecurityLabel::Internal, SecurityLabel::Sensitive);
    assert!(result.is_ok());
}

/// Regression test 8: Planner in template requests disallowed tool → kernel rejects.
#[test]
fn test_capability_denied_for_disallowed_tool() {
    let engine = PolicyEngine::with_defaults();

    // Create a task with restricted tool set (from scheduling template).
    let task = pfar::types::Task {
        task_id: uuid::Uuid::nil(),
        template_id: "whatsapp_scheduling".to_owned(),
        principal: Principal::WhatsAppContact("+34665030077".to_owned()),
        trigger_event: uuid::Uuid::nil(),
        data_ceiling: SecurityLabel::Internal,
        allowed_tools: vec!["calendar.freebusy".to_owned(), "message.reply".to_owned()],
        denied_tools: vec!["email.send".to_owned()],
        max_tool_calls: 5,
        output_sinks: vec!["sink:whatsapp:reply_to_sender".to_owned()],
        trace_id: "test".to_owned(),
        state: pfar::types::TaskState::Executing { current_step: 0 },
    };

    let taint = pfar::types::TaintSet {
        level: TaintLevel::Raw,
        origin: "whatsapp:+34665030077".to_owned(),
        touched_by: vec![],
    };

    // Allowed tool → OK.
    assert!(engine
        .issue_capability(&task, "calendar.freebusy", "cal".to_owned(), taint.clone())
        .is_ok());

    // Denied tool → error.
    assert!(engine
        .issue_capability(&task, "email.send", "acct".to_owned(), taint.clone())
        .is_err());

    // Tool not in allowed list → error.
    assert!(engine
        .issue_capability(&task, "github.create_issue", "repo".to_owned(), taint)
        .is_err());
}

/// Regression test 7: Regulated health data cannot egress to WhatsApp.
#[test]
fn test_regulated_health_data_blocked_from_whatsapp() {
    let engine = PolicyEngine::with_defaults();

    // WhatsApp sink is Public level.
    let whatsapp_label = engine
        .sink_label("sink:whatsapp:reply_to_sender")
        .expect("should have whatsapp sink");
    assert_eq!(whatsapp_label, SecurityLabel::Public);

    // Regulated data → public sink → DENIED.
    assert!(engine
        .check_write(SecurityLabel::Regulated, whatsapp_label)
        .is_err());

    // Telegram owner sink is Regulated level (can receive health data).
    let telegram_label = engine
        .sink_label("sink:telegram:owner")
        .expect("should have telegram sink");
    assert_eq!(telegram_label, SecurityLabel::Regulated);

    // Regulated data → regulated sink → ALLOWED (owner can see their own health data).
    assert!(engine
        .check_write(SecurityLabel::Regulated, telegram_label)
        .is_ok());
}

/// Load templates from directory (tests the TOML loading path).
#[test]
fn test_load_templates_from_directory() {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    std::fs::write(dir.path().join("owner.toml"), OWNER_TEMPLATE).expect("write");
    std::fs::write(dir.path().join("scheduling.toml"), SCHEDULING_TEMPLATE).expect("write");

    let registry = TemplateRegistry::load_from_dir(dir.path()).expect("should load templates");
    assert!(registry.get("owner_cli_general").is_some());
    assert!(registry.get("whatsapp_scheduling").is_some());
}
