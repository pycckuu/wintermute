//! LLM-based diagnosis for novel problems not caught by pattern matching.
//!
//! Uses the Flatline's own model (cheap/local) via ModelRouter. Budget is
//! checked before every LLM call. Response is redacted before parsing.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use wintermute::agent::budget::DailyBudget;
use wintermute::executor::redactor::Redactor;
use wintermute::heartbeat::health::HealthReport;
use wintermute::providers::router::ModelRouter;
use wintermute::providers::{CompletionRequest, ContentPart};

use crate::patterns::GitLogEntry;
use crate::watcher::LogEvent;

/// Estimated tokens per diagnosis call (for budget pre-check).
const ESTIMATED_DIAGNOSIS_TOKENS: u64 = 1000;

/// Maximum number of log events to include in the evidence prompt.
const MAX_LOG_EVENTS: usize = 50;

/// Maximum character length for the evidence string sent to the LLM.
const MAX_EVIDENCE_CHARS: usize = 8000;

/// Structured diagnosis from the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnosis {
    /// Root cause in one sentence.
    pub root_cause: String,
    /// Confidence level.
    pub confidence: DiagnosisConfidence,
    /// Recommended action.
    pub recommended_action: String,
    /// Additional details.
    pub details: String,
}

/// Confidence level for a diagnosis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosisConfidence {
    /// Strong signal, high certainty.
    High,
    /// Probable but not certain.
    Medium,
    /// Weak signal, speculative.
    Low,
}

/// System prompt for the LLM diagnostician.
const DIAGNOSIS_SYSTEM_PROMPT: &str = "\
You are a system diagnostician. Analyze these events and identify the likely root cause.

Respond with a JSON object:
{
  \"root_cause\": \"one sentence\",
  \"confidence\": \"high\" | \"medium\" | \"low\",
  \"recommended_action\": \"revert_commit\" | \"quarantine_tool\" | \"restart_process\" | \"reset_sandbox\" | \"report_only\",
  \"details\": \"what specifically to do\"
}

Output ONLY the JSON object, no other text.";

/// Diagnose a novel problem using LLM analysis.
///
/// Only called when no known pattern matches but anomalies exist.
/// Rules first, LLM second.
///
/// Returns `None` if the LLM's confidence is low (Flatline reports instead
/// of acting on low-confidence diagnoses) or if no diagnosis could be produced.
///
/// # Errors
///
/// Returns error if provider unavailable, budget exceeded, or LLM call fails.
pub async fn diagnose(
    log_events: &[LogEvent],
    health: Option<&HealthReport>,
    git_log: &[GitLogEntry],
    tool_stats: &[(String, f64)],
    router: &ModelRouter,
    redactor: &Redactor,
    daily_budget: &DailyBudget,
) -> anyhow::Result<Option<Diagnosis>> {
    // Step 1: Resolve the flatline model (falls back through chain).
    let provider = router
        .resolve(Some("flatline"), None)
        .context("failed to resolve flatline model")?;

    // Step 2: Budget pre-check.
    daily_budget
        .check(ESTIMATED_DIAGNOSIS_TOKENS)
        .context("flatline diagnosis budget exceeded")?;

    debug!(model = %provider.model_id(), "flatline diagnosis starting");

    // Step 3: Build evidence string from all inputs.
    let evidence = build_evidence(log_events, health, git_log, tool_stats);

    // Step 4: Build CompletionRequest with system prompt + evidence as user message.
    let request = CompletionRequest {
        messages: vec![wintermute::providers::Message {
            role: wintermute::providers::Role::User,
            content: wintermute::providers::MessageContent::Text(evidence),
        }],
        system: Some(DIAGNOSIS_SYSTEM_PROMPT.to_owned()),
        tools: vec![],
        max_tokens: Some(1024),
        stop_sequences: vec![],
    };

    // Step 5: Call the provider.
    let response = provider
        .complete(request)
        .await
        .context("flatline diagnosis LLM call failed")?;

    // Step 6: Record actual token usage.
    let total = u64::from(response.usage.input_tokens)
        .saturating_add(u64::from(response.usage.output_tokens));
    daily_budget.record(total);

    // Step 7: Extract text from response content parts.
    let response_text = response
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    if response_text.is_empty() {
        debug!("flatline diagnosis received empty response");
        return Ok(None);
    }

    // Step 8: Redact before parsing (security invariant #7).
    let redacted = redactor.redact(&response_text);

    // Step 9: Parse JSON response into Diagnosis struct.
    let diagnosis = match parse_diagnosis(&redacted) {
        Some(d) => d,
        None => {
            debug!("flatline diagnosis could not parse LLM response");
            return Ok(None);
        }
    };

    // Step 10: If confidence is Low, return None (report instead of acting).
    if diagnosis.confidence == DiagnosisConfidence::Low {
        debug!(
            root_cause = %diagnosis.root_cause,
            "low-confidence diagnosis, skipping action"
        );
        return Ok(None);
    }

    Ok(Some(diagnosis))
}

/// Parse diagnosis JSON from LLM response, returning None on parse failure.
///
/// Tries to find `{...}` in the response text (LLM may include extra text),
/// then parses with `serde_json`.
pub fn parse_diagnosis(text: &str) -> Option<Diagnosis> {
    let trimmed = text.trim();

    // Try to find JSON object in the response.
    let json_text = if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                &trimmed[start..=end]
            } else {
                trimmed
            }
        } else {
            trimmed
        }
    } else {
        trimmed
    };

    match serde_json::from_str::<Diagnosis>(json_text) {
        Ok(d) => Some(d),
        Err(e) => {
            warn!(
                error = %e,
                text_preview = &text[..text.len().min(200)],
                "failed to parse diagnosis JSON"
            );
            None
        }
    }
}

/// Build the evidence string from log events, health, git log, and tool stats.
///
/// Truncates to a reasonable size to avoid excessive token usage.
fn build_evidence(
    log_events: &[LogEvent],
    health: Option<&HealthReport>,
    git_log: &[GitLogEntry],
    tool_stats: &[(String, f64)],
) -> String {
    let mut evidence = String::with_capacity(MAX_EVIDENCE_CHARS);

    // Section: Recent Events
    evidence.push_str("## Recent Events\n");
    let events_to_show = if log_events.len() > MAX_LOG_EVENTS {
        &log_events[log_events.len().saturating_sub(MAX_LOG_EVENTS)..]
    } else {
        log_events
    };
    for event in events_to_show {
        let level = event.level.as_deref().unwrap_or("?");
        let ts = event.ts.as_deref().unwrap_or("?");
        let evt = event.event.as_deref().unwrap_or("?");
        let tool = event.tool.as_deref().unwrap_or("-");
        let err = event
            .error
            .as_deref()
            .map(|e| format!(" error={e}"))
            .unwrap_or_default();
        evidence.push_str(&format!("[{ts}] {level} {evt} tool={tool}{err}\n"));

        if evidence.len() > MAX_EVIDENCE_CHARS {
            evidence.push_str("...[truncated]\n");
            break;
        }
    }

    // Section: Recent Changes
    evidence.push_str("\n## Recent Changes\n");
    for entry in git_log.iter().take(10) {
        let short_hash = &entry.hash[..7.min(entry.hash.len())];
        evidence.push_str(&format!(
            "{} {} {}\n",
            short_hash, entry.timestamp, entry.message
        ));
    }

    // Section: Current Health
    evidence.push_str("\n## Current Health\n");
    if let Some(h) = health {
        evidence.push_str(&format!("status: {}\n", h.status));
        evidence.push_str(&format!("uptime: {}s\n", h.uptime_secs));
        evidence.push_str(&format!("container_healthy: {}\n", h.container_healthy));
        evidence.push_str(&format!(
            "budget: {}/{}\n",
            h.budget_today.used, h.budget_today.limit
        ));
        if let Some(err) = &h.last_error {
            evidence.push_str(&format!("last_error: {err}\n"));
        }
    } else {
        evidence.push_str("health.json not available\n");
    }

    // Section: Tool Stats
    evidence.push_str("\n## Tool Stats\n");
    if tool_stats.is_empty() {
        evidence.push_str("no tool failure data\n");
    } else {
        for (tool, rate) in tool_stats {
            evidence.push_str(&format!("{tool}: {:.0}% failure rate\n", rate * 100.0));
        }
    }

    // Final truncation safety net.
    if evidence.len() > MAX_EVIDENCE_CHARS {
        evidence.truncate(MAX_EVIDENCE_CHARS);
        evidence.push_str("\n...[truncated]");
    }

    evidence
}
