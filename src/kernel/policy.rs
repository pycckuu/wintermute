/// Policy Engine — mandatory access control enforcement (spec 6.2).
///
/// Enforces the information flow lattice, graduated taint rules,
/// capability token generation/validation, and sink access control.
/// This is the security core of PFAR; all 10 privacy invariants
/// depend on correct policy enforcement.
use std::collections::HashMap;

use chrono::{Duration, Utc};
use thiserror::Error;
use uuid::Uuid;

use crate::types::{
    ApprovalDecision, CapabilityToken, EventSource, Principal, PrincipalClass, SecurityLabel,
    TaintLevel, TaintSet, Task,
};

/// Policy violation — data flow rule broken (spec 6.2).
#[derive(Debug, Error)]
pub enum PolicyViolation {
    /// No Write Down: data at level X cannot flow to sink below X.
    #[error(
        "No Write Down violation: data label {data_label:?} exceeds sink label {sink_label:?}"
    )]
    NoWriteDown {
        data_label: SecurityLabel,
        sink_label: SecurityLabel,
    },
    /// Inference routing denied: data ceiling prevents sending to cloud provider (spec 11.1).
    #[error("inference routing denied: {label:?} data cannot be sent to cloud provider")]
    InferenceRoutingDenied {
        /// The security label that triggered the denial.
        label: SecurityLabel,
    },
}

/// Policy error — capability or tool access denied (spec 6.2).
#[derive(Debug, Error)]
pub enum PolicyError {
    /// Tool not in the template's allowed_tools list.
    #[error("tool '{tool}' not in template's allowed_tools")]
    ToolNotAllowed { tool: String },
    /// Tool is explicitly denied by the template.
    #[error("tool '{tool}' is in template's denied_tools")]
    ToolDenied { tool: String },
    /// Capability token has expired.
    #[error("capability token expired")]
    CapabilityExpired,
    /// Capability token task_id doesn't match the current task.
    #[error("capability token task_id mismatch")]
    CapabilityTaskMismatch,
    /// Capability token is for a different tool than the one being invoked.
    #[error("capability token is for tool '{actual}', not '{expected}'")]
    CapabilityToolMismatch { expected: String, actual: String },
}

/// Policy Engine enforcing MAC lattice and taint rules (spec 6.2).
///
/// The kernel instantiates one `PolicyEngine` at startup. All label
/// assignments, taint checks, and capability operations go through it.
pub struct PolicyEngine {
    /// Kernel-defined label ceilings per tool (overrides tool self-reports).
    label_ceilings: HashMap<String, SecurityLabel>,
    /// Security labels assigned to sinks.
    sink_labels: HashMap<String, SecurityLabel>,
}

impl PolicyEngine {
    /// Create a policy engine with the given label ceilings and sink labels.
    pub fn new(
        label_ceilings: HashMap<String, SecurityLabel>,
        sink_labels: HashMap<String, SecurityLabel>,
    ) -> Self {
        Self {
            label_ceilings,
            sink_labels,
        }
    }

    /// Create a policy engine with default sink labels from spec 4.7.
    pub fn with_defaults() -> Self {
        let mut sink_labels = HashMap::new();
        // Owner's primary sink at Regulated so health data can egress (spec 4.7, regression test 7).
        sink_labels.insert("sink:telegram:owner".to_owned(), SecurityLabel::Regulated);
        sink_labels.insert("sink:notion:*".to_owned(), SecurityLabel::Sensitive);
        sink_labels.insert("sink:slack:owner_dm".to_owned(), SecurityLabel::Sensitive);
        sink_labels.insert(
            "sink:whatsapp:reply_to_sender".to_owned(),
            SecurityLabel::Public,
        );
        sink_labels.insert("sink:github:public".to_owned(), SecurityLabel::Public);
        sink_labels.insert("sink:github:private".to_owned(), SecurityLabel::Internal);

        let mut label_ceilings = HashMap::new();
        label_ceilings.insert("calendar.freebusy".to_owned(), SecurityLabel::Internal);
        label_ceilings.insert("calendar.list_events".to_owned(), SecurityLabel::Sensitive);
        label_ceilings.insert("email.list".to_owned(), SecurityLabel::Sensitive);
        label_ceilings.insert("email.read".to_owned(), SecurityLabel::Sensitive);
        label_ceilings.insert("github.list_prs".to_owned(), SecurityLabel::Sensitive);

        Self::new(label_ceilings, sink_labels)
    }

    // ── Label assignment (spec 4.3 table) ──

    /// Assign security label to an event based on provenance (spec 4.3).
    pub fn assign_event_label(&self, source: &EventSource) -> SecurityLabel {
        match &source.principal {
            Principal::Owner => SecurityLabel::Sensitive,
            Principal::TelegramPeer(_) => SecurityLabel::Internal,
            Principal::SlackUser { .. } => SecurityLabel::Internal,
            Principal::WhatsAppContact(_) => SecurityLabel::Internal,
            Principal::Webhook(_) => SecurityLabel::Sensitive,
            Principal::Cron(_) => SecurityLabel::Sensitive,
        }
    }

    /// Assign taint set to an event based on source (spec 4.4).
    pub fn assign_event_taint(&self, source: &EventSource) -> TaintSet {
        match &source.principal {
            Principal::Owner => TaintSet {
                level: TaintLevel::Clean,
                origin: "owner".to_owned(),
                touched_by: vec![],
            },
            Principal::TelegramPeer(id) => TaintSet {
                level: TaintLevel::Raw,
                origin: format!("adapter:telegram:peer:{id}"),
                touched_by: vec![],
            },
            Principal::SlackUser {
                workspace,
                channel,
                user,
            } => TaintSet {
                level: TaintLevel::Raw,
                origin: format!("adapter:slack:{workspace}:{channel}:{user}"),
                touched_by: vec![],
            },
            Principal::WhatsAppContact(phone) => TaintSet {
                level: TaintLevel::Raw,
                origin: format!("adapter:whatsapp:{phone}"),
                touched_by: vec![],
            },
            Principal::Webhook(source_name) => TaintSet {
                level: TaintLevel::Raw,
                origin: format!("webhook:{source_name}"),
                touched_by: vec![],
            },
            Principal::Cron(job) => TaintSet {
                level: TaintLevel::Clean,
                origin: format!("cron:{job}"),
                touched_by: vec![],
            },
        }
    }

    // ── Label propagation (spec 4.3) ──

    /// Propagate labels: result inherits max of all inputs (spec 4.3).
    pub fn propagate_label(&self, labels: &[SecurityLabel]) -> SecurityLabel {
        labels
            .iter()
            .copied()
            .max()
            .unwrap_or(SecurityLabel::Public)
    }

    // ── Access control checks (spec 6.2) ──

    /// No Read Up: can a subject at `subject_level` read data at `object_level`? (spec 6.2).
    pub fn check_read(&self, subject_level: SecurityLabel, object_level: SecurityLabel) -> bool {
        subject_level >= object_level
    }

    /// No Write Down: can data at `data_label` flow to a sink at `sink_label`? (spec 6.2).
    pub fn check_write(
        &self,
        data_label: SecurityLabel,
        sink_label: SecurityLabel,
    ) -> Result<(), PolicyViolation> {
        if data_label > sink_label {
            return Err(PolicyViolation::NoWriteDown {
                data_label,
                sink_label,
            });
        }
        Ok(())
    }

    /// Resolve the security label for a named sink.
    pub fn sink_label(&self, sink: &str) -> Option<SecurityLabel> {
        // Try exact match first, then wildcard patterns.
        if let Some(label) = self.sink_labels.get(sink) {
            return Some(*label);
        }
        // Check wildcard entries (e.g. "sink:notion:*" matches "sink:notion:digest").
        for (pattern, label) in &self.sink_labels {
            if let Some(prefix) = pattern.strip_suffix('*') {
                if sink.starts_with(prefix) {
                    return Some(*label);
                }
            }
        }
        None
    }

    // ── Label ceiling (spec 6.2) ──

    /// Apply kernel-defined label ceiling to a tool's result (spec 6.2).
    ///
    /// The kernel's ceiling is the authoritative label for data produced
    /// by this tool. If no ceiling is defined, the reported label is
    /// used as-is.
    pub fn apply_label_ceiling(&self, tool: &str, reported_label: SecurityLabel) -> SecurityLabel {
        match self.label_ceilings.get(tool) {
            Some(&ceiling) => ceiling,
            None => reported_label,
        }
    }

    // ── Inference routing (spec 11.1) ──

    /// Check if inference routing is allowed for a given data ceiling and provider (spec 11.1).
    ///
    /// Routing rules:
    /// - `Public`/`Internal`: any provider allowed
    /// - `Sensitive`: local only unless `cloud_risk_ack` is true
    /// - `Regulated`: always local, cannot be overridden
    /// - `Secret`: never sent to any LLM
    pub fn check_inference_routing(
        &self,
        data_ceiling: SecurityLabel,
        provider_is_cloud: bool,
        cloud_risk_ack: bool,
    ) -> Result<(), PolicyViolation> {
        // Secret data must never be sent to any LLM (spec 11.1).
        if data_ceiling == SecurityLabel::Secret {
            return Err(PolicyViolation::InferenceRoutingDenied {
                label: data_ceiling,
            });
        }

        // Local providers are always permitted for non-secret data.
        if !provider_is_cloud {
            return Ok(());
        }

        // Cloud provider routing checks by label.
        match data_ceiling {
            SecurityLabel::Public | SecurityLabel::Internal => Ok(()),
            SecurityLabel::Sensitive => {
                if cloud_risk_ack {
                    Ok(())
                } else {
                    Err(PolicyViolation::InferenceRoutingDenied {
                        label: data_ceiling,
                    })
                }
            }
            SecurityLabel::Regulated => Err(PolicyViolation::InferenceRoutingDenied {
                label: data_ceiling,
            }),
            SecurityLabel::Secret => Err(PolicyViolation::InferenceRoutingDenied {
                label: data_ceiling,
            }),
        }
    }

    // ── Taint checking (spec 4.4, graduated rules) ──

    /// Check graduated taint rules for write operations (spec 4.4).
    ///
    /// | Taint level | Structured only | Free-text content |
    /// |-------------|-----------------|-------------------|
    /// | Raw         | Approval        | Approval          |
    /// | Extracted   | Auto-approved   | Approval          |
    /// | Clean       | Auto-approved   | Auto-approved     |
    pub fn check_taint(&self, taint: &TaintSet, has_free_text: bool) -> ApprovalDecision {
        match taint.level {
            TaintLevel::Raw => ApprovalDecision::RequiresHumanApproval {
                reason: format!(
                    "raw external content from '{}' requires approval for writes",
                    taint.origin
                ),
            },
            TaintLevel::Extracted => {
                if has_free_text {
                    ApprovalDecision::RequiresHumanApproval {
                        reason: format!(
                            "extracted content from '{}' with free-text fields requires approval",
                            taint.origin
                        ),
                    }
                } else {
                    ApprovalDecision::AutoApproved
                }
            }
            TaintLevel::Clean => ApprovalDecision::AutoApproved,
        }
    }

    // ── Capability tokens (spec 4.6) ──

    /// Issue a capability token for a tool invocation (spec 4.6).
    ///
    /// Validates that the tool is permitted by the task's template before
    /// issuing the token.
    pub fn issue_capability(
        &self,
        task: &Task,
        tool: &str,
        resource_scope: String,
        arg_taint: TaintSet,
    ) -> Result<CapabilityToken, PolicyError> {
        // Check denied_tools first (takes precedence).
        if task.denied_tools.iter().any(|d| d == tool || d == "*") {
            return Err(PolicyError::ToolDenied {
                tool: tool.to_owned(),
            });
        }

        // Check allowed_tools (supports wildcard "admin.*").
        let allowed = task.allowed_tools.iter().any(|a| {
            a == tool
                || a == "*"
                || a.strip_suffix(".*").is_some_and(|prefix| {
                    tool.starts_with(prefix) && tool.as_bytes().get(prefix.len()) == Some(&b'.')
                })
        });
        if !allowed {
            return Err(PolicyError::ToolNotAllowed {
                tool: tool.to_owned(),
            });
        }

        let now = Utc::now();
        // Capability tokens are short-lived: 5 minutes (spec 4.6).
        let expires_at = now
            .checked_add_signed(Duration::minutes(5))
            .ok_or(PolicyError::CapabilityExpired)?;
        Ok(CapabilityToken {
            capability_id: Uuid::new_v4(),
            task_id: task.task_id,
            template_id: task.template_id.clone(),
            principal: task.principal.clone(),
            tool: tool.to_owned(),
            resource_scope,
            taint_of_arguments: arg_taint,
            issued_at: now,
            expires_at,
            max_invocations: 1,
        })
    }

    /// Validate a capability token against a task and specific tool (spec 4.6).
    pub fn validate_capability(
        &self,
        token: &CapabilityToken,
        task: &Task,
        tool: &str,
    ) -> Result<(), PolicyError> {
        if token.task_id != task.task_id {
            return Err(PolicyError::CapabilityTaskMismatch);
        }
        if token.tool != tool {
            return Err(PolicyError::CapabilityToolMismatch {
                expected: tool.to_owned(),
                actual: token.tool.clone(),
            });
        }
        if Utc::now() > token.expires_at {
            return Err(PolicyError::CapabilityExpired);
        }
        Ok(())
    }
}

/// Resolve principal class from principal (spec 4.1).
pub fn resolve_principal_class(principal: &Principal) -> PrincipalClass {
    match principal {
        Principal::Owner => PrincipalClass::Owner,
        Principal::TelegramPeer(_) => PrincipalClass::ThirdParty,
        Principal::SlackUser { .. } => PrincipalClass::ThirdParty,
        Principal::WhatsAppContact(_) => PrincipalClass::ThirdParty,
        Principal::Webhook(_) => PrincipalClass::WebhookSource,
        Principal::Cron(_) => PrincipalClass::Cron,
    }
}

/// Format a trigger string from adapter, event kind, and principal class (spec 6.1).
pub fn format_trigger(adapter: &str, kind: &str, principal_class: PrincipalClass) -> String {
    let class_str = match principal_class {
        PrincipalClass::Owner => "owner",
        PrincipalClass::Paired => "paired",
        PrincipalClass::ThirdParty => "third_party",
        PrincipalClass::WebhookSource => "webhook",
        PrincipalClass::Cron => "cron",
    };
    format!("adapter:{adapter}:{kind}:{class_str}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TaskState;

    fn test_engine() -> PolicyEngine {
        PolicyEngine::with_defaults()
    }

    fn test_task() -> Task {
        Task {
            task_id: Uuid::nil(),
            template_id: "test".to_owned(),
            principal: Principal::Owner,
            trigger_event: Uuid::nil(),
            data_ceiling: SecurityLabel::Sensitive,
            allowed_tools: vec![
                "email.list".to_owned(),
                "email.read".to_owned(),
                "admin.*".to_owned(),
            ],
            denied_tools: vec!["email.send_as_owner".to_owned()],
            max_tool_calls: 10,
            output_sinks: vec!["sink:telegram:owner".to_owned()],
            trace_id: "test-trace".to_owned(),
            state: TaskState::Extracting,
        }
    }

    // ── Label assignment tests ──

    #[test]
    fn test_assign_label_owner() {
        let engine = test_engine();
        let source = EventSource {
            adapter: "telegram".to_owned(),
            principal: Principal::Owner,
        };
        assert_eq!(engine.assign_event_label(&source), SecurityLabel::Sensitive);
    }

    #[test]
    fn test_assign_label_telegram_peer() {
        let engine = test_engine();
        let source = EventSource {
            adapter: "telegram".to_owned(),
            principal: Principal::TelegramPeer("12345".to_owned()),
        };
        assert_eq!(engine.assign_event_label(&source), SecurityLabel::Internal);
    }

    #[test]
    fn test_assign_label_webhook() {
        let engine = test_engine();
        let source = EventSource {
            adapter: "webhook".to_owned(),
            principal: Principal::Webhook("fireflies".to_owned()),
        };
        assert_eq!(engine.assign_event_label(&source), SecurityLabel::Sensitive);
    }

    #[test]
    fn test_assign_label_cron() {
        let engine = test_engine();
        let source = EventSource {
            adapter: "cron".to_owned(),
            principal: Principal::Cron("email_check".to_owned()),
        };
        assert_eq!(engine.assign_event_label(&source), SecurityLabel::Sensitive);
    }

    // ── Taint assignment tests ──

    #[test]
    fn test_assign_taint_owner_is_clean() {
        let engine = test_engine();
        let source = EventSource {
            adapter: "telegram".to_owned(),
            principal: Principal::Owner,
        };
        let taint = engine.assign_event_taint(&source);
        assert_eq!(taint.level, TaintLevel::Clean);
    }

    #[test]
    fn test_assign_taint_third_party_is_raw() {
        let engine = test_engine();
        let source = EventSource {
            adapter: "telegram".to_owned(),
            principal: Principal::TelegramPeer("12345".to_owned()),
        };
        let taint = engine.assign_event_taint(&source);
        assert_eq!(taint.level, TaintLevel::Raw);
        assert!(taint.origin.contains("12345"));
    }

    #[test]
    fn test_assign_taint_webhook_is_raw() {
        let engine = test_engine();
        let source = EventSource {
            adapter: "webhook".to_owned(),
            principal: Principal::Webhook("fireflies".to_owned()),
        };
        let taint = engine.assign_event_taint(&source);
        assert_eq!(taint.level, TaintLevel::Raw);
        assert!(taint.origin.contains("fireflies"));
    }

    // ── Label propagation tests ──

    #[test]
    fn test_propagate_label_max() {
        let engine = test_engine();
        let result = engine.propagate_label(&[SecurityLabel::Internal, SecurityLabel::Sensitive]);
        assert_eq!(result, SecurityLabel::Sensitive);
    }

    #[test]
    fn test_propagate_label_empty() {
        let engine = test_engine();
        let result = engine.propagate_label(&[]);
        assert_eq!(result, SecurityLabel::Public);
    }

    #[test]
    fn test_propagate_label_single() {
        let engine = test_engine();
        let result = engine.propagate_label(&[SecurityLabel::Regulated]);
        assert_eq!(result, SecurityLabel::Regulated);
    }

    // ── No Read Up tests ──

    #[test]
    fn test_check_read_allowed() {
        let engine = test_engine();
        assert!(engine.check_read(SecurityLabel::Sensitive, SecurityLabel::Internal));
        assert!(engine.check_read(SecurityLabel::Internal, SecurityLabel::Internal));
    }

    #[test]
    fn test_check_read_denied() {
        let engine = test_engine();
        assert!(!engine.check_read(SecurityLabel::Internal, SecurityLabel::Sensitive));
        assert!(!engine.check_read(SecurityLabel::Public, SecurityLabel::Secret));
    }

    // ── No Write Down tests ──

    #[test]
    fn test_check_write_allowed() {
        let engine = test_engine();
        assert!(engine
            .check_write(SecurityLabel::Internal, SecurityLabel::Sensitive)
            .is_ok());
        assert!(engine
            .check_write(SecurityLabel::Sensitive, SecurityLabel::Sensitive)
            .is_ok());
    }

    #[test]
    fn test_check_write_denied() {
        let engine = test_engine();
        let result = engine.check_write(SecurityLabel::Sensitive, SecurityLabel::Public);
        assert!(matches!(result, Err(PolicyViolation::NoWriteDown { .. })));
    }

    #[test]
    fn test_check_write_regulated_to_public() {
        let engine = test_engine();
        let result = engine.check_write(SecurityLabel::Regulated, SecurityLabel::Public);
        assert!(matches!(result, Err(PolicyViolation::NoWriteDown { .. })));
    }

    // ── Graduated taint tests (spec 4.4) ──

    #[test]
    fn test_taint_raw_structured_requires_approval() {
        let engine = test_engine();
        let taint = TaintSet {
            level: TaintLevel::Raw,
            origin: "webhook:fireflies".to_owned(),
            touched_by: vec![],
        };
        assert!(matches!(
            engine.check_taint(&taint, false),
            ApprovalDecision::RequiresHumanApproval { .. }
        ));
    }

    #[test]
    fn test_taint_raw_freetext_requires_approval() {
        let engine = test_engine();
        let taint = TaintSet {
            level: TaintLevel::Raw,
            origin: "webhook:fireflies".to_owned(),
            touched_by: vec![],
        };
        assert!(matches!(
            engine.check_taint(&taint, true),
            ApprovalDecision::RequiresHumanApproval { .. }
        ));
    }

    #[test]
    fn test_taint_extracted_structured_auto() {
        let engine = test_engine();
        let taint = TaintSet {
            level: TaintLevel::Extracted,
            origin: "webhook:fireflies".to_owned(),
            touched_by: vec!["extractor:transcript".to_owned()],
        };
        assert!(matches!(
            engine.check_taint(&taint, false),
            ApprovalDecision::AutoApproved
        ));
    }

    #[test]
    fn test_taint_extracted_freetext_requires_approval() {
        let engine = test_engine();
        let taint = TaintSet {
            level: TaintLevel::Extracted,
            origin: "webhook:fireflies".to_owned(),
            touched_by: vec!["extractor:transcript".to_owned()],
        };
        assert!(matches!(
            engine.check_taint(&taint, true),
            ApprovalDecision::RequiresHumanApproval { .. }
        ));
    }

    #[test]
    fn test_taint_clean_always_auto() {
        let engine = test_engine();
        let taint = TaintSet {
            level: TaintLevel::Clean,
            origin: "owner".to_owned(),
            touched_by: vec![],
        };
        assert!(matches!(
            engine.check_taint(&taint, false),
            ApprovalDecision::AutoApproved
        ));
        assert!(matches!(
            engine.check_taint(&taint, true),
            ApprovalDecision::AutoApproved
        ));
    }

    // ── Capability token tests ──

    #[test]
    fn test_issue_capability_allowed_tool() {
        let engine = test_engine();
        let task = test_task();
        let taint = TaintSet {
            level: TaintLevel::Clean,
            origin: "owner".to_owned(),
            touched_by: vec![],
        };
        let cap = engine
            .issue_capability(&task, "email.list", "account:personal".to_owned(), taint)
            .expect("should issue");
        assert_eq!(cap.tool, "email.list");
        assert_eq!(cap.task_id, task.task_id);
        assert_eq!(cap.max_invocations, 1);
    }

    #[test]
    fn test_issue_capability_wildcard_allowed() {
        let engine = test_engine();
        let task = test_task();
        let taint = TaintSet {
            level: TaintLevel::Clean,
            origin: "owner".to_owned(),
            touched_by: vec![],
        };
        // "admin.*" in allowed_tools should match "admin.list_integrations"
        let cap = engine
            .issue_capability(&task, "admin.list_integrations", "system".to_owned(), taint)
            .expect("should issue via wildcard");
        assert_eq!(cap.tool, "admin.list_integrations");
    }

    #[test]
    fn test_issue_capability_denied_tool() {
        let engine = test_engine();
        let task = test_task();
        let taint = TaintSet {
            level: TaintLevel::Clean,
            origin: "owner".to_owned(),
            touched_by: vec![],
        };
        let result = engine.issue_capability(
            &task,
            "email.send_as_owner",
            "account:personal".to_owned(),
            taint,
        );
        assert!(matches!(result, Err(PolicyError::ToolDenied { .. })));
    }

    #[test]
    fn test_issue_capability_not_in_allowed() {
        let engine = test_engine();
        let task = test_task();
        let taint = TaintSet {
            level: TaintLevel::Clean,
            origin: "owner".to_owned(),
            touched_by: vec![],
        };
        let result =
            engine.issue_capability(&task, "github.create_issue", "repo:x".to_owned(), taint);
        assert!(matches!(result, Err(PolicyError::ToolNotAllowed { .. })));
    }

    #[test]
    fn test_validate_capability_valid() {
        let engine = test_engine();
        let task = test_task();
        let taint = TaintSet {
            level: TaintLevel::Clean,
            origin: "owner".to_owned(),
            touched_by: vec![],
        };
        let cap = engine
            .issue_capability(&task, "email.list", "account:personal".to_owned(), taint)
            .expect("issue");
        assert!(engine
            .validate_capability(&cap, &task, "email.list")
            .is_ok());
    }

    #[test]
    fn test_validate_capability_task_mismatch() {
        let engine = test_engine();
        let task = test_task();
        let taint = TaintSet {
            level: TaintLevel::Clean,
            origin: "owner".to_owned(),
            touched_by: vec![],
        };
        let cap = engine
            .issue_capability(&task, "email.list", "account:personal".to_owned(), taint)
            .expect("issue");

        let mut other_task = test_task();
        other_task.task_id = Uuid::new_v4();
        assert!(matches!(
            engine.validate_capability(&cap, &other_task, "email.list"),
            Err(PolicyError::CapabilityTaskMismatch)
        ));
    }

    #[test]
    fn test_validate_capability_tool_mismatch() {
        let engine = test_engine();
        let task = test_task();
        let taint = TaintSet {
            level: TaintLevel::Clean,
            origin: "owner".to_owned(),
            touched_by: vec![],
        };
        let cap = engine
            .issue_capability(&task, "email.list", "account:personal".to_owned(), taint)
            .expect("issue");
        // Use token issued for email.list to try to authorize email.read.
        assert!(matches!(
            engine.validate_capability(&cap, &task, "email.read"),
            Err(PolicyError::CapabilityToolMismatch { .. })
        ));
    }

    #[test]
    fn test_validate_capability_expired() {
        let engine = test_engine();
        let task = test_task();
        let taint = TaintSet {
            level: TaintLevel::Clean,
            origin: "owner".to_owned(),
            touched_by: vec![],
        };
        let mut cap = engine
            .issue_capability(&task, "email.list", "account:personal".to_owned(), taint)
            .expect("issue");
        // Expire it manually.
        cap.expires_at = Utc::now() - Duration::minutes(1);
        assert!(matches!(
            engine.validate_capability(&cap, &task, "email.list"),
            Err(PolicyError::CapabilityExpired)
        ));
    }

    // ── Label ceiling tests ──

    #[test]
    fn test_apply_ceiling_override() {
        let engine = test_engine();
        // calendar.freebusy has ceiling Internal (spec says freebusy is declassified).
        // Kernel ceiling is authoritative — tool's report is ignored.
        let result = engine.apply_label_ceiling("calendar.freebusy", SecurityLabel::Public);
        assert_eq!(result, SecurityLabel::Internal);
    }

    #[test]
    fn test_apply_ceiling_no_override() {
        let engine = test_engine();
        // Unknown tool — no ceiling defined, use reported.
        let result = engine.apply_label_ceiling("some.unknown.tool", SecurityLabel::Public);
        assert_eq!(result, SecurityLabel::Public);
    }

    #[test]
    fn test_apply_ceiling_caps_higher_reported() {
        let engine = test_engine();
        // Kernel ceiling is authoritative — even if tool reports higher, ceiling wins.
        let result = engine.apply_label_ceiling("calendar.freebusy", SecurityLabel::Sensitive);
        assert_eq!(result, SecurityLabel::Internal);
    }

    // ── Sink label resolution tests ──

    #[test]
    fn test_sink_label_exact() {
        let engine = test_engine();
        assert_eq!(
            engine.sink_label("sink:telegram:owner"),
            Some(SecurityLabel::Regulated)
        );
    }

    #[test]
    fn test_sink_label_wildcard() {
        let engine = test_engine();
        assert_eq!(
            engine.sink_label("sink:notion:digest"),
            Some(SecurityLabel::Sensitive)
        );
    }

    #[test]
    fn test_sink_label_unknown() {
        let engine = test_engine();
        assert_eq!(engine.sink_label("sink:unknown:foo"), None);
    }

    // ── Principal resolution tests ──

    #[test]
    fn test_resolve_principal_class_owner() {
        assert_eq!(
            resolve_principal_class(&Principal::Owner),
            PrincipalClass::Owner
        );
    }

    #[test]
    fn test_resolve_principal_class_telegram() {
        assert_eq!(
            resolve_principal_class(&Principal::TelegramPeer("1".to_owned())),
            PrincipalClass::ThirdParty
        );
    }

    #[test]
    fn test_resolve_principal_class_webhook() {
        assert_eq!(
            resolve_principal_class(&Principal::Webhook("x".to_owned())),
            PrincipalClass::WebhookSource
        );
    }

    #[test]
    fn test_resolve_principal_class_cron() {
        assert_eq!(
            resolve_principal_class(&Principal::Cron("job".to_owned())),
            PrincipalClass::Cron
        );
    }

    // ── Trigger formatting tests ──

    #[test]
    fn test_format_trigger_owner() {
        let t = format_trigger("telegram", "message", PrincipalClass::Owner);
        assert_eq!(t, "adapter:telegram:message:owner");
    }

    #[test]
    fn test_format_trigger_third_party() {
        let t = format_trigger("whatsapp", "message", PrincipalClass::ThirdParty);
        assert_eq!(t, "adapter:whatsapp:message:third_party");
    }

    #[test]
    fn test_format_trigger_webhook() {
        let t = format_trigger("webhook", "post", PrincipalClass::WebhookSource);
        assert_eq!(t, "adapter:webhook:post:webhook");
    }

    // ── Inference routing tests (spec 11.1) ──

    #[test]
    fn test_inference_routing_public_cloud_ok() {
        let engine = test_engine();
        assert!(engine
            .check_inference_routing(SecurityLabel::Public, true, false)
            .is_ok());
    }

    #[test]
    fn test_inference_routing_internal_cloud_ok() {
        let engine = test_engine();
        assert!(engine
            .check_inference_routing(SecurityLabel::Internal, true, false)
            .is_ok());
    }

    #[test]
    fn test_inference_routing_sensitive_cloud_denied() {
        let engine = test_engine();
        let result = engine.check_inference_routing(SecurityLabel::Sensitive, true, false);
        assert!(matches!(
            result,
            Err(PolicyViolation::InferenceRoutingDenied { .. })
        ));
    }

    #[test]
    fn test_inference_routing_sensitive_cloud_with_ack() {
        let engine = test_engine();
        assert!(engine
            .check_inference_routing(SecurityLabel::Sensitive, true, true)
            .is_ok());
    }

    #[test]
    fn test_inference_routing_sensitive_local_ok() {
        let engine = test_engine();
        assert!(engine
            .check_inference_routing(SecurityLabel::Sensitive, false, false)
            .is_ok());
    }

    #[test]
    fn test_inference_routing_regulated_always_local() {
        let engine = test_engine();
        // Regulated to cloud denied even with ack.
        let result = engine.check_inference_routing(SecurityLabel::Regulated, true, true);
        assert!(matches!(
            result,
            Err(PolicyViolation::InferenceRoutingDenied { .. })
        ));
        // Regulated to local is fine.
        assert!(engine
            .check_inference_routing(SecurityLabel::Regulated, false, false)
            .is_ok());
    }

    #[test]
    fn test_inference_routing_secret_always_denied() {
        let engine = test_engine();
        // Secret to any LLM (cloud or local) is denied.
        let result_cloud = engine.check_inference_routing(SecurityLabel::Secret, true, true);
        assert!(matches!(
            result_cloud,
            Err(PolicyViolation::InferenceRoutingDenied { .. })
        ));
        let result_local = engine.check_inference_routing(SecurityLabel::Secret, false, false);
        assert!(matches!(
            result_local,
            Err(PolicyViolation::InferenceRoutingDenied { .. })
        ));
    }
}
