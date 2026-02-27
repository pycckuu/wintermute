//! Task brief lifecycle and persistence.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tracing::trace;

use super::MessagingError;

/// Row type returned by SQLite queries for task briefs.
type BriefRow = (
    String,
    String,
    Option<i64>,
    String,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// A task brief describing an outbound messaging objective.
///
/// Briefs define what the agent is allowed to share, negotiate, and commit to
/// when communicating with external contacts on behalf of the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskBrief {
    /// Unique identifier for this brief.
    pub id: String,
    /// Session that created this brief.
    pub session_id: String,
    /// Optional linked contact.
    pub contact_id: Option<i64>,
    /// What the agent is trying to accomplish.
    pub objective: String,
    /// Information explicitly approved for sharing.
    pub shareable_info: Vec<String>,
    /// Negotiation constraints and boundaries.
    pub constraints: Vec<Constraint>,
    /// Conditions that should trigger escalation to the user.
    pub escalation_triggers: Vec<String>,
    /// How much authority the agent has.
    pub commitment_level: CommitmentLevel,
    /// Optional tone guidance for the conversation.
    pub tone: Option<String>,
    /// Current lifecycle status.
    pub status: BriefStatus,
    /// Summary of the outcome when completed.
    pub outcome_summary: Option<String>,
    /// When the brief was created.
    pub created_at: Option<String>,
    /// When the brief was completed or cancelled.
    pub completed_at: Option<String>,
}

/// A negotiation constraint attached to a brief.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Constraint {
    /// Budget range with a starting offer and private ceiling.
    Budget {
        /// Starting offer amount.
        start: f64,
        /// Maximum amount (private, never shared).
        ceiling: f64,
        /// Currency code.
        currency: String,
    },
    /// Acceptable time window.
    TimeWindow {
        /// Earliest acceptable time.
        earliest: String,
        /// Latest acceptable time.
        latest: String,
    },
    /// Something that must be included.
    MustInclude(String),
    /// Something that must be avoided.
    MustAvoid(String),
    /// A freeform constraint.
    Custom(String),
}

/// How much authority the agent has to make commitments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentLevel {
    /// Agent can confirm, agree, or book if constraints are met.
    CanCommit,
    /// Agent can negotiate but must escalate to finalize.
    NegotiateOnly,
    /// Agent only gathers information, no commitments.
    InformationOnly,
}

impl CommitmentLevel {
    /// Returns the SQLite-stored string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CanCommit => "can_commit",
            Self::NegotiateOnly => "negotiate_only",
            Self::InformationOnly => "information_only",
        }
    }

    /// Parse a string into a commitment level.
    ///
    /// # Errors
    ///
    /// Returns [`MessagingError::InvalidTransition`] if the string is unrecognized.
    pub fn parse(s: &str) -> Result<Self, MessagingError> {
        match s {
            "can_commit" => Ok(Self::CanCommit),
            "negotiate_only" => Ok(Self::NegotiateOnly),
            "information_only" => Ok(Self::InformationOnly),
            other => Err(MessagingError::InvalidTransition {
                from: other.to_owned(),
                to: "CommitmentLevel".to_owned(),
            }),
        }
    }
}

/// Lifecycle status of a task brief.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BriefStatus {
    /// Just created, not yet confirmed by the user.
    Draft,
    /// User confirmed the brief parameters.
    Confirmed,
    /// Agent is actively communicating.
    Active,
    /// Agent escalated to the user for guidance.
    Escalated,
    /// Agent proposed a deal/agreement to the contact.
    Proposed,
    /// Contact accepted a proposal.
    Committed,
    /// Task finished successfully.
    Completed,
    /// Task was cancelled.
    Cancelled,
}

impl BriefStatus {
    /// Returns the SQLite-stored string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Confirmed => "confirmed",
            Self::Active => "active",
            Self::Escalated => "escalated",
            Self::Proposed => "proposed",
            Self::Committed => "committed",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse a string into a brief status.
    ///
    /// # Errors
    ///
    /// Returns [`MessagingError::InvalidTransition`] if the string is unrecognized.
    pub fn parse(s: &str) -> Result<Self, MessagingError> {
        match s {
            "draft" => Ok(Self::Draft),
            "confirmed" => Ok(Self::Confirmed),
            "active" => Ok(Self::Active),
            "escalated" => Ok(Self::Escalated),
            "proposed" => Ok(Self::Proposed),
            "committed" => Ok(Self::Committed),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(MessagingError::InvalidTransition {
                from: other.to_owned(),
                to: "BriefStatus".to_owned(),
            }),
        }
    }

    /// Check if transitioning to `target` is valid.
    pub fn can_transition_to(&self, target: BriefStatus) -> bool {
        matches!(
            (self, target),
            (Self::Draft, BriefStatus::Confirmed)
                | (Self::Draft, BriefStatus::Cancelled)
                | (Self::Confirmed, BriefStatus::Active)
                | (Self::Confirmed, BriefStatus::Cancelled)
                | (Self::Active, BriefStatus::Escalated)
                | (Self::Active, BriefStatus::Proposed)
                | (Self::Active, BriefStatus::Completed)
                | (Self::Active, BriefStatus::Cancelled)
                | (Self::Escalated, BriefStatus::Active)
                | (Self::Escalated, BriefStatus::Cancelled)
                | (Self::Proposed, BriefStatus::Committed)
                | (Self::Proposed, BriefStatus::Active)
                | (Self::Proposed, BriefStatus::Cancelled)
                | (Self::Committed, BriefStatus::Completed)
                | (Self::Committed, BriefStatus::Cancelled)
        )
    }
}

/// Format a summary of active briefs for system prompt injection.
///
/// Returns an empty string if there are no briefs, so it can be safely
/// checked with `is_empty()` before injecting into the prompt.
pub fn active_briefs_summary(briefs: &[TaskBrief]) -> String {
    if briefs.is_empty() {
        return String::new();
    }
    let mut summary = String::from("## Active Task Briefs\n");
    for brief in briefs {
        let contact_label = brief
            .contact_id
            .map_or_else(|| "no contact".to_owned(), |id| format!("contact #{id}"));
        summary.push_str(&format!(
            "- [{}] {}: {} (status: {})\n",
            brief.id,
            contact_label,
            brief.objective,
            brief.status.as_str()
        ));
    }
    summary
}

/// Convert a `BriefRow` tuple into a [`TaskBrief`], propagating parse errors.
fn brief_from_row(row: BriefRow) -> Result<TaskBrief, MessagingError> {
    Ok(TaskBrief {
        id: row.0,
        session_id: row.1,
        contact_id: row.2,
        objective: row.3,
        shareable_info: serde_json::from_str(&row.4).unwrap_or_default(),
        constraints: serde_json::from_str(&row.5).unwrap_or_default(),
        escalation_triggers: row
            .6
            .as_deref()
            .map(|s| serde_json::from_str(s).unwrap_or_default())
            .unwrap_or_default(),
        commitment_level: CommitmentLevel::parse(&row.7)?,
        tone: row.8,
        status: BriefStatus::parse(&row.9)?,
        outcome_summary: row.10,
        created_at: row.11,
        completed_at: row.12,
    })
}

/// Convert a `BriefRow` into a [`TaskBrief`], using defaults for fields that fail to parse.
fn brief_from_row_lenient(row: BriefRow) -> TaskBrief {
    TaskBrief {
        id: row.0,
        session_id: row.1,
        contact_id: row.2,
        objective: row.3,
        shareable_info: serde_json::from_str(&row.4).unwrap_or_default(),
        constraints: serde_json::from_str(&row.5).unwrap_or_default(),
        escalation_triggers: row
            .6
            .as_deref()
            .map(|s| serde_json::from_str(s).unwrap_or_default())
            .unwrap_or_default(),
        commitment_level: CommitmentLevel::parse(&row.7).unwrap_or(CommitmentLevel::NegotiateOnly),
        tone: row.8,
        status: BriefStatus::parse(&row.9).unwrap_or(BriefStatus::Active),
        outcome_summary: row.10,
        created_at: row.11,
        completed_at: row.12,
    }
}

/// Insert a new brief into SQLite.
///
/// # Errors
///
/// Returns [`MessagingError::Database`] on SQLite failure.
pub async fn insert_brief(db: &SqlitePool, brief: &TaskBrief) -> Result<(), MessagingError> {
    let shareable_json = serde_json::to_string(&brief.shareable_info).unwrap_or_default();
    let constraints_json = serde_json::to_string(&brief.constraints).unwrap_or_default();
    let triggers_json = serde_json::to_string(&brief.escalation_triggers).unwrap_or_default();

    sqlx::query(
        "INSERT INTO task_briefs (id, session_id, contact_id, objective, shareable_info, \
         constraints, escalation_triggers, commitment_level, tone, status) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )
    .bind(&brief.id)
    .bind(&brief.session_id)
    .bind(brief.contact_id)
    .bind(&brief.objective)
    .bind(&shareable_json)
    .bind(&constraints_json)
    .bind(&triggers_json)
    .bind(brief.commitment_level.as_str())
    .bind(&brief.tone)
    .bind(brief.status.as_str())
    .execute(db)
    .await?;

    trace!(brief_id = %brief.id, "brief inserted");
    Ok(())
}

/// Load a brief by ID.
///
/// # Errors
///
/// Returns [`MessagingError::BriefNotFound`] if no brief matches,
/// or [`MessagingError::Database`] on SQLite failure.
pub async fn load_brief(db: &SqlitePool, brief_id: &str) -> Result<TaskBrief, MessagingError> {
    let row: BriefRow = sqlx::query_as(
        "SELECT id, session_id, contact_id, objective, shareable_info, constraints, \
         escalation_triggers, commitment_level, tone, status, outcome_summary, \
         created_at, completed_at \
         FROM task_briefs WHERE id = ?1",
    )
    .bind(brief_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| MessagingError::BriefNotFound(brief_id.to_owned()))?;

    brief_from_row(row)
}

/// Update brief status with transition validation.
///
/// # Errors
///
/// Returns [`MessagingError::InvalidTransition`] if the transition is not allowed,
/// [`MessagingError::BriefNotFound`] if the brief does not exist,
/// or [`MessagingError::Database`] on SQLite failure.
pub async fn update_brief_status(
    db: &SqlitePool,
    brief_id: &str,
    new_status: BriefStatus,
    outcome: Option<&str>,
) -> Result<(), MessagingError> {
    let brief = load_brief(db, brief_id).await?;
    if !brief.status.can_transition_to(new_status) {
        return Err(MessagingError::InvalidTransition {
            from: brief.status.as_str().to_owned(),
            to: new_status.as_str().to_owned(),
        });
    }

    let completed_at = if matches!(new_status, BriefStatus::Completed | BriefStatus::Cancelled) {
        Some(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string())
    } else {
        None
    };

    sqlx::query(
        "UPDATE task_briefs SET status = ?1, outcome_summary = COALESCE(?2, outcome_summary), \
         completed_at = COALESCE(?3, completed_at), updated_at = datetime('now') WHERE id = ?4",
    )
    .bind(new_status.as_str())
    .bind(outcome)
    .bind(&completed_at)
    .bind(brief_id)
    .execute(db)
    .await?;

    trace!(
        brief_id,
        status = new_status.as_str(),
        "brief status updated"
    );
    Ok(())
}

/// Load all active briefs across all sessions.
///
/// Returns briefs with status `active`, `escalated`, `proposed`, or `committed`,
/// regardless of which session created them. Useful for proactive monitoring.
///
/// # Errors
///
/// Returns [`MessagingError::Database`] on SQLite failure.
pub async fn all_active_briefs(db: &SqlitePool) -> Result<Vec<TaskBrief>, MessagingError> {
    let rows: Vec<BriefRow> = sqlx::query_as(
        "SELECT id, session_id, contact_id, objective, shareable_info, constraints, \
         escalation_triggers, commitment_level, tone, status, outcome_summary, \
         created_at, completed_at \
         FROM task_briefs \
         WHERE status IN ('active', 'escalated', 'proposed', 'committed') \
         ORDER BY created_at DESC",
    )
    .fetch_all(db)
    .await?;

    Ok(rows.into_iter().map(brief_from_row_lenient).collect())
}

/// Load all active briefs for a session.
///
/// Returns briefs with status `active`, `escalated`, `proposed`, or `committed`.
///
/// # Errors
///
/// Returns [`MessagingError::Database`] on SQLite failure.
pub async fn active_briefs_for_session(
    db: &SqlitePool,
    session_id: &str,
) -> Result<Vec<TaskBrief>, MessagingError> {
    let rows: Vec<BriefRow> = sqlx::query_as(
        "SELECT id, session_id, contact_id, objective, shareable_info, constraints, \
         escalation_triggers, commitment_level, tone, status, outcome_summary, \
         created_at, completed_at \
         FROM task_briefs WHERE session_id = ?1 \
         AND status IN ('active', 'escalated', 'proposed', 'committed') \
         ORDER BY created_at DESC",
    )
    .bind(session_id)
    .fetch_all(db)
    .await?;

    Ok(rows.into_iter().map(brief_from_row_lenient).collect())
}
