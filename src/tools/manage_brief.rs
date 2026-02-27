//! manage_brief tool: create, update, and manage task briefs.

use sqlx::SqlitePool;

use crate::messaging::brief::{self, BriefStatus, CommitmentLevel, Constraint, TaskBrief};

use super::ToolError;

/// Handle the manage_brief tool call.
///
/// Supports actions: `create`, `update`, `escalate`, `propose`, `complete`, `cancel`.
///
/// # Errors
///
/// Returns [`ToolError::InvalidInput`] for missing or invalid fields,
/// or [`ToolError::ExecutionFailed`] on database or state-transition failure.
pub async fn manage_brief(
    db: &SqlitePool,
    session_id: &str,
    input: &serde_json::Value,
) -> Result<String, ToolError> {
    let action = input
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: action".to_owned()))?;

    match action {
        "create" => create_brief(db, session_id, input).await,
        "update" => update_brief(db, input).await,
        "escalate" => transition_brief(db, input, BriefStatus::Escalated).await,
        "propose" => transition_brief(db, input, BriefStatus::Proposed).await,
        "complete" => complete_brief(db, input).await,
        "cancel" => transition_brief(db, input, BriefStatus::Cancelled).await,
        other => Err(ToolError::InvalidInput(format!("unknown action: {other}"))),
    }
}

/// Create a new task brief in draft status.
async fn create_brief(
    db: &SqlitePool,
    session_id: &str,
    input: &serde_json::Value,
) -> Result<String, ToolError> {
    let objective = input
        .get("objective")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: objective".to_owned()))?;

    let shareable_info: Vec<String> = input
        .get("shareable_info")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let constraints: Vec<Constraint> = input
        .get("constraints")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let commitment_level = input
        .get("commitment_level")
        .and_then(|v| v.as_str())
        .map(CommitmentLevel::parse)
        .transpose()
        .map_err(|e| ToolError::InvalidInput(e.to_string()))?
        .unwrap_or(CommitmentLevel::NegotiateOnly);

    let escalation_triggers: Vec<String> = input
        .get("escalation_triggers")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let tone = input
        .get("tone")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());

    let brief_id = generate_brief_id();
    let brief = TaskBrief {
        id: brief_id.clone(),
        session_id: session_id.to_owned(),
        contact_id: None,
        objective: objective.to_owned(),
        shareable_info,
        constraints,
        escalation_triggers,
        commitment_level,
        tone,
        status: BriefStatus::Draft,
        outcome_summary: None,
        created_at: None,
        completed_at: None,
    };

    brief::insert_brief(db, &brief)
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

    Ok(format!(
        "Brief created with id: {brief_id}. Status: draft. Confirm with the user before starting."
    ))
}

/// Update mutable fields of an existing brief.
async fn update_brief(db: &SqlitePool, input: &serde_json::Value) -> Result<String, ToolError> {
    let brief_id = input
        .get("brief_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: brief_id".to_owned()))?;

    let mut brief = brief::load_brief(db, brief_id)
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

    if let Some(objective) = input.get("objective").and_then(|v| v.as_str()) {
        brief.objective = objective.to_owned();
    }
    if let Some(info) = input
        .get("shareable_info")
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
    {
        brief.shareable_info = info;
    }
    if let Some(constraints) = input
        .get("constraints")
        .and_then(|v| serde_json::from_value::<Vec<Constraint>>(v.clone()).ok())
    {
        brief.constraints = constraints;
    }
    if let Some(tone) = input.get("tone").and_then(|v| v.as_str()) {
        brief.tone = Some(tone.to_owned());
    }

    sqlx::query(
        "UPDATE task_briefs SET objective=?1, shareable_info=?2, constraints=?3, \
         tone=?4, updated_at=datetime('now') WHERE id=?5",
    )
    .bind(&brief.objective)
    .bind(serde_json::to_string(&brief.shareable_info).unwrap_or_default())
    .bind(serde_json::to_string(&brief.constraints).unwrap_or_default())
    .bind(&brief.tone)
    .bind(brief_id)
    .execute(db)
    .await
    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

    Ok(format!("Brief {brief_id} updated."))
}

/// Transition a brief to a target status.
async fn transition_brief(
    db: &SqlitePool,
    input: &serde_json::Value,
    target: BriefStatus,
) -> Result<String, ToolError> {
    let brief_id = input
        .get("brief_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: brief_id".to_owned()))?;

    let outcome = input.get("outcome_summary").and_then(|v| v.as_str());
    let reason = input.get("escalation_reason").and_then(|v| v.as_str());
    let summary = outcome.or(reason);

    brief::update_brief_status(db, brief_id, target, summary)
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

    Ok(format!(
        "Brief {brief_id} status changed to {}.",
        target.as_str()
    ))
}

/// Complete a brief with an optional outcome summary.
async fn complete_brief(db: &SqlitePool, input: &serde_json::Value) -> Result<String, ToolError> {
    let brief_id = input
        .get("brief_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: brief_id".to_owned()))?;

    let outcome = input.get("outcome_summary").and_then(|v| v.as_str());

    brief::update_brief_status(db, brief_id, BriefStatus::Completed, outcome)
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

    Ok(format!("Brief {brief_id} completed."))
}

/// Generate a random 12-char brief ID with `brief_` prefix.
fn generate_brief_id() -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::thread_rng();
    let id: String = (0..12)
        .map(|_| {
            let idx = rng.gen_range(0..CHARS.len());
            CHARS[idx] as char
        })
        .collect();
    format!("brief_{id}")
}
