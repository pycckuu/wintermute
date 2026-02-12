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
