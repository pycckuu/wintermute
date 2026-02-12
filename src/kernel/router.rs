/// Event Router — receives events, matches templates, creates tasks (spec 6.1).
///
/// The router is the entry point into the kernel. It:
/// 1. Assigns security labels and taint tags based on provenance
/// 2. Resolves the principal class
/// 3. Matches the event to a task template
/// 4. Creates a Task bound to the template's capability ceiling
/// 5. Logs the task creation via the audit logger
use std::sync::Arc;

use thiserror::Error;
use uuid::Uuid;

use crate::kernel::audit::AuditLogger;
use crate::kernel::policy::{format_trigger, resolve_principal_class, PolicyEngine};
use crate::kernel::template::TemplateRegistry;
use crate::types::{InboundEvent, LabeledEvent, Task, TaskState};

/// Router error types (spec 6.1).
#[derive(Debug, Error)]
pub enum RouterError {
    /// No template matched the event's trigger and principal class.
    #[error("no template matches event from {adapter} ({principal:?})")]
    NoTemplateMatch {
        adapter: String,
        principal: crate::types::Principal,
    },
    /// Audit logging failed.
    #[error("audit error: {0}")]
    AuditError(#[from] anyhow::Error),
}

/// Event router coordinating event dispatch (spec 6.1).
pub struct EventRouter {
    policy: Arc<PolicyEngine>,
    templates: Arc<TemplateRegistry>,
    audit: Arc<AuditLogger>,
}

impl EventRouter {
    /// Create a new event router.
    pub fn new(
        policy: Arc<PolicyEngine>,
        templates: Arc<TemplateRegistry>,
        audit: Arc<AuditLogger>,
    ) -> Self {
        Self {
            policy,
            templates,
            audit,
        }
    }

    /// Process an inbound event through the kernel (spec 6.1).
    ///
    /// Returns a `LabeledEvent` and a `Task` ready for pipeline execution.
    pub fn route_event(&self, event: InboundEvent) -> Result<(LabeledEvent, Task), RouterError> {
        // 1. Assign security label based on provenance (spec 4.3).
        let label = self.policy.assign_event_label(&event.source);

        // 2. Assign taint tags based on source (spec 4.4).
        let taint = self.policy.assign_event_taint(&event.source);

        // 3. Resolve principal class (spec 4.1).
        let principal_class = resolve_principal_class(&event.source.principal);

        // 4. Format trigger string for template matching.
        let kind_str = match &event.kind {
            crate::types::EventKind::Message => "message",
            crate::types::EventKind::Command => "command",
            crate::types::EventKind::Callback => "callback",
            crate::types::EventKind::Webhook => "webhook",
            crate::types::EventKind::CronTrigger => "cron",
            crate::types::EventKind::CredentialReply => "credential_reply",
        };
        let trigger = format_trigger(&event.source.adapter, kind_str, principal_class);

        // 5. Match template (spec 6.1).
        let template = self
            .templates
            .match_template(&trigger, principal_class)
            .ok_or_else(|| RouterError::NoTemplateMatch {
                adapter: event.source.adapter.clone(),
                principal: event.source.principal.clone(),
            })?;

        // 6. Create Task from template (spec 10.2).
        let task = Task {
            task_id: Uuid::new_v4(),
            template_id: template.template_id.clone(),
            principal: event.source.principal.clone(),
            trigger_event: event.event_id,
            data_ceiling: template.data_ceiling,
            allowed_tools: template.allowed_tools.clone(),
            denied_tools: template.denied_tools.clone(),
            max_tool_calls: template.max_tool_calls,
            output_sinks: template.output_sinks.clone(),
            trace_id: Uuid::new_v4().to_string(),
            state: TaskState::Extracting,
        };

        // 7. Audit log task creation (spec 6.7).
        self.audit.log_task_created(&task)?;

        let labeled = LabeledEvent {
            event,
            label,
            taint,
        };

        Ok((labeled, task))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::audit::AuditLogger;
    use crate::kernel::template::TemplateRegistry;
    use crate::types::{EventKind, EventPayload, EventSource, Principal, SecurityLabel};
    use std::io::Cursor;
    use std::sync::Mutex;

    fn test_router() -> EventRouter {
        let policy = Arc::new(PolicyEngine::with_defaults());
        let audit = Arc::new(AuditLogger::from_writer(Box::new(SharedBuf::new())));

        let mut templates = TemplateRegistry::new();
        let owner_template: crate::kernel::template::TaskTemplate =
            toml::from_str(OWNER_TEMPLATE).expect("parse owner template");
        let tp_template: crate::kernel::template::TaskTemplate =
            toml::from_str(THIRD_PARTY_TEMPLATE).expect("parse tp template");
        templates.register(owner_template);
        templates.register(tp_template);

        EventRouter::new(policy, Arc::new(templates), audit)
    }

    fn owner_event() -> InboundEvent {
        InboundEvent {
            event_id: Uuid::nil(),
            timestamp: chrono::Utc::now(),
            source: EventSource {
                adapter: "telegram".to_owned(),
                principal: Principal::Owner,
            },
            kind: EventKind::Message,
            payload: EventPayload {
                text: Some("Check my email".to_owned()),
                attachments: vec![],
                reply_to: None,
                metadata: serde_json::json!({}),
            },
        }
    }

    fn third_party_event() -> InboundEvent {
        InboundEvent {
            event_id: Uuid::nil(),
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
        }
    }

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

    const OWNER_TEMPLATE: &str = r#"
template_id = "owner_telegram_general"
triggers = ["adapter:telegram:message:owner"]
principal_class = "owner"
description = "General assistant for owner via Telegram"
allowed_tools = ["email.list", "email.read", "calendar.freebusy"]
max_tool_calls = 15
output_sinks = ["sink:telegram:owner"]
data_ceiling = "sensitive"

[inference]
provider = "local"
model = "llama3"
"#;

    const THIRD_PARTY_TEMPLATE: &str = r#"
template_id = "whatsapp_scheduling"
triggers = ["adapter:whatsapp:message:third_party"]
principal_class = "third_party"
description = "Handle scheduling requests"
allowed_tools = ["calendar.freebusy", "message.reply"]
max_tool_calls = 5
output_sinks = ["sink:whatsapp:reply_to_sender"]
data_ceiling = "internal"

[inference]
provider = "local"
model = "llama3"
"#;

    #[test]
    fn test_route_owner_event() {
        let router = test_router();
        let (labeled, task) = router.route_event(owner_event()).expect("should route");

        assert_eq!(task.template_id, "owner_telegram_general");
        assert_eq!(task.principal, Principal::Owner);
        assert_eq!(task.data_ceiling, SecurityLabel::Sensitive);
        assert!(task.allowed_tools.contains(&"email.list".to_owned()));
        assert_eq!(labeled.label, SecurityLabel::Sensitive);
        assert_eq!(labeled.taint.level, crate::types::TaintLevel::Clean);
    }

    #[test]
    fn test_route_third_party_event() {
        let router = test_router();
        let (labeled, task) = router
            .route_event(third_party_event())
            .expect("should route");

        assert_eq!(task.template_id, "whatsapp_scheduling");
        assert_eq!(task.data_ceiling, SecurityLabel::Internal);
        assert!(task.allowed_tools.contains(&"calendar.freebusy".to_owned()));
        assert_eq!(labeled.label, SecurityLabel::Internal);
        assert_eq!(labeled.taint.level, crate::types::TaintLevel::Raw);
    }

    #[test]
    fn test_route_no_matching_template() {
        let router = test_router();
        let event = InboundEvent {
            event_id: Uuid::nil(),
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
        assert!(matches!(result, Err(RouterError::NoTemplateMatch { .. })));
    }

    #[test]
    fn test_task_starts_in_extracting_state() {
        let router = test_router();
        let (_, task) = router.route_event(owner_event()).expect("should route");
        assert!(matches!(task.state, TaskState::Extracting));
    }
}
