// Core types for PFAR v2 (spec sections 4.1–4.7)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Canonical identity for an external actor (spec 4.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Principal {
    /// System owner — highest trust.
    Owner,
    /// Telegram peer identified by user ID.
    TelegramPeer(String),
    /// Slack user in workspace/channel.
    SlackUser {
        workspace: String,
        channel: String,
        user: String,
    },
    /// WhatsApp contact identified by phone number.
    WhatsAppContact(String),
    /// Authenticated webhook source.
    Webhook(String),
    /// Scheduled job running as owner context.
    Cron(String),
}

/// Principal trust class (spec 4.1 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalClass {
    /// System-level scheduled jobs.
    Cron,
    /// Untrusted input from webhooks.
    WebhookSource,
    /// Untrusted third-party users.
    ThirdParty,
    /// Semi-trusted paired users.
    Paired,
    /// Fully trusted system owner.
    Owner,
}

/// Security levels ordered lowest to highest (spec 4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecurityLabel {
    /// From the open internet.
    Public,
    /// Semi-trusted workspace or external human input.
    Internal,
    /// Private correspondence, calendar, code.
    Sensitive,
    /// Medical/personal data.
    Regulated,
    /// API keys, OAuth tokens — never egress.
    Secret,
}

impl std::fmt::Display for SecurityLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Public => f.write_str("public"),
            Self::Internal => f.write_str("internal"),
            Self::Sensitive => f.write_str("sensitive"),
            Self::Regulated => f.write_str("regulated"),
            Self::Secret => f.write_str("secret"),
        }
    }
}

impl std::str::FromStr for SecurityLabel {
    type Err = anyhow::Error;

    /// Parse a security label from its lowercase string representation
    /// (feature-dynamic-integrations, spec 4.3).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "public" => Ok(Self::Public),
            "internal" => Ok(Self::Internal),
            "sensitive" => Ok(Self::Sensitive),
            "regulated" => Ok(Self::Regulated),
            "secret" => Ok(Self::Secret),
            other => Err(anyhow::anyhow!("unknown security label: {other}")),
        }
    }
}

/// Taint level tracking sanitization state (spec 4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TaintLevel {
    /// Owner-generated or owner-approved content.
    Clean,
    /// Passed through a structured extractor.
    Extracted,
    /// Raw external content — full taint.
    Raw,
}

/// Taint metadata tracking data provenance (spec 4.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaintSet {
    /// Current taint level.
    pub level: TaintLevel,
    /// Origin identifier (e.g. "webhook:fireflies", "adapter:telegram:peer:12345").
    pub origin: String,
    /// Chain of processors that have touched this data.
    pub touched_by: Vec<String>,
}

/// Capability token authorizing a single tool invocation (spec 4.6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityToken {
    pub capability_id: Uuid,
    pub task_id: Uuid,
    pub template_id: String,
    pub principal: Principal,
    pub tool: String,
    pub resource_scope: String,
    pub taint_of_arguments: TaintSet,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub max_invocations: u32,
}

/// Task state machine (spec 10.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskState {
    Extracting,
    Planning,
    Executing { current_step: usize },
    Synthesizing,
    AwaitingApproval { step: usize, reason: String },
    AwaitingCredential { service: String },
    Completed,
    Failed { error: String },
}

/// A task instantiated from a template (spec 10.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub task_id: Uuid,
    pub template_id: String,
    pub principal: Principal,
    pub trigger_event: Uuid,
    pub data_ceiling: SecurityLabel,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    pub max_tool_calls: u32,
    pub output_sinks: Vec<String>,
    pub trace_id: String,
    pub state: TaskState,
}

/// Normalized inbound event from an adapter (spec 10.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundEvent {
    pub event_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub source: EventSource,
    pub kind: EventKind,
    pub payload: EventPayload,
}

/// Event source with adapter and principal info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSource {
    pub adapter: String,
    pub principal: Principal,
}

/// Event classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventKind {
    Message,
    Command,
    Callback,
    Webhook,
    CronTrigger,
    CredentialReply,
}

/// Event payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventPayload {
    pub text: Option<String>,
    pub attachments: Vec<String>,
    pub reply_to: Option<String>,
    pub metadata: serde_json::Value,
}

/// Event with security labels and taint assigned by the kernel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabeledEvent {
    pub event: InboundEvent,
    pub label: SecurityLabel,
    pub taint: TaintSet,
}

/// Policy engine decision on whether approval is needed.
#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    AutoApproved,
    RequiresHumanApproval { reason: String },
}

/// Tool invocation result (spec 10.6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub invocation_id: Uuid,
    pub success: bool,
    pub output: serde_json::Value,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_security_label_from_str() {
        assert_eq!(
            "public".parse::<SecurityLabel>().expect("ok"),
            SecurityLabel::Public
        );
        assert_eq!(
            "internal".parse::<SecurityLabel>().expect("ok"),
            SecurityLabel::Internal
        );
        assert_eq!(
            "sensitive".parse::<SecurityLabel>().expect("ok"),
            SecurityLabel::Sensitive
        );
        assert_eq!(
            "regulated".parse::<SecurityLabel>().expect("ok"),
            SecurityLabel::Regulated
        );
        assert_eq!(
            "secret".parse::<SecurityLabel>().expect("ok"),
            SecurityLabel::Secret
        );
    }

    #[test]
    fn test_security_label_from_str_case_insensitive() {
        assert_eq!(
            "Public".parse::<SecurityLabel>().expect("ok"),
            SecurityLabel::Public
        );
        assert_eq!(
            "INTERNAL".parse::<SecurityLabel>().expect("ok"),
            SecurityLabel::Internal
        );
        assert_eq!(
            "Sensitive".parse::<SecurityLabel>().expect("ok"),
            SecurityLabel::Sensitive
        );
    }

    #[test]
    fn test_security_label_from_str_invalid() {
        assert!("bogus".parse::<SecurityLabel>().is_err());
        assert!("".parse::<SecurityLabel>().is_err());
    }

    #[test]
    fn test_security_label_display() {
        assert_eq!(SecurityLabel::Public.to_string(), "public");
        assert_eq!(SecurityLabel::Internal.to_string(), "internal");
        assert_eq!(SecurityLabel::Sensitive.to_string(), "sensitive");
        assert_eq!(SecurityLabel::Regulated.to_string(), "regulated");
        assert_eq!(SecurityLabel::Secret.to_string(), "secret");
    }

    #[test]
    fn test_security_label_roundtrip() {
        for label in &[
            SecurityLabel::Public,
            SecurityLabel::Internal,
            SecurityLabel::Sensitive,
            SecurityLabel::Regulated,
            SecurityLabel::Secret,
        ] {
            let s = label.to_string();
            let parsed: SecurityLabel = s.parse().expect("roundtrip should work");
            assert_eq!(*label, parsed);
        }
    }
}
