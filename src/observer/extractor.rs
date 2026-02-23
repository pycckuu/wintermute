//! LLM-based extraction of facts and procedures from conversation history.
//!
//! Uses the observer model (cheap/local) to analyze conversation transcripts
//! and extract learnable information. All output is redacted before parsing.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::agent::budget::DailyBudget;
use crate::executor::redactor::Redactor;
use crate::providers::router::ModelRouter;
use crate::providers::{CompletionRequest, ContentPart, Message, MessageContent, Role};

/// Estimated tokens per observer extraction call (for budget pre-check).
const ESTIMATED_EXTRACTION_TOKENS: u64 = 500;

/// Minimum confidence threshold for keeping an extraction.
const MIN_CONFIDENCE: f64 = 0.5;

/// Kind of extracted information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtractionKind {
    /// A declarative fact about the user or their environment.
    Fact,
    /// A step-by-step procedure or workflow.
    Procedure,
    /// A user preference or behavioral pattern.
    Preference,
}

/// A single extraction from conversation analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Extraction {
    /// What kind of information this is.
    pub kind: ExtractionKind,
    /// The extracted content.
    pub content: String,
    /// Confidence score (0.0–1.0) from the LLM.
    pub confidence: f64,
}

/// System prompt for the observer extraction model.
const EXTRACTION_SYSTEM_PROMPT: &str = "\
You are an observer that extracts learnable facts and procedures from conversations.
Analyze the conversation and output a JSON array of extractions.
Each extraction must be an object with these fields:
- \"kind\": one of \"fact\", \"procedure\", or \"preference\"
- \"content\": a concise, self-contained description of the learned information
- \"confidence\": a float between 0.0 and 1.0 indicating how confident you are

Only extract genuinely useful, non-obvious information. Be conservative.
Do not extract greetings, small talk, or trivial observations.
Output ONLY the JSON array, no other text. If nothing is worth extracting, output [].";

/// Extract facts and procedures from conversation messages.
///
/// Uses the observer model (resolved via `ModelRouter::resolve(Some("observer"), None)`)
/// to analyze the conversation. Budget is checked before the LLM call.
/// The response is redacted before parsing.
///
/// Returns an empty vec on parse failure (logged as warning, never panics).
///
/// # Errors
///
/// Returns an error if the provider is unavailable, budget is exceeded,
/// or the LLM call fails.
pub async fn extract(
    messages: &[Message],
    router: &ModelRouter,
    redactor: &Redactor,
    daily_budget: &DailyBudget,
) -> anyhow::Result<Vec<Extraction>> {
    // Resolve the observer model (cheap/local).
    let provider = router
        .resolve(Some("observer"), None)
        .context("failed to resolve observer model")?;

    // Budget pre-check.
    daily_budget
        .check(ESTIMATED_EXTRACTION_TOKENS)
        .context("observer budget exceeded")?;

    debug!(model = %provider.model_id(), "observer extraction starting");

    // Build the extraction request (no tools — text-only output).
    let request = CompletionRequest {
        messages: messages.to_vec(),
        system: Some(EXTRACTION_SYSTEM_PROMPT.to_owned()),
        tools: vec![],
        max_tokens: Some(2048),
        stop_sequences: vec![],
    };

    let response = provider
        .complete(request)
        .await
        .context("observer LLM call failed")?;

    // Record actual token usage.
    let total = u64::from(response.usage.input_tokens)
        .saturating_add(u64::from(response.usage.output_tokens));
    daily_budget.record(total);

    // Extract text from response.
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
        debug!("observer received empty response");
        return Ok(Vec::new());
    }

    // Redact before parsing (security invariant #7).
    let redacted = redactor.redact(&response_text);

    // Parse JSON array of extractions.
    parse_extractions(&redacted)
}

/// Parse extraction JSON, filtering by confidence threshold.
///
/// Returns an empty vec on any parse error (logged as warning).
pub fn parse_extractions(text: &str) -> anyhow::Result<Vec<Extraction>> {
    // Try to find JSON array in the response (LLM may include extra text).
    let trimmed = text.trim();
    let json_text = if let Some(start) = trimmed.find('[') {
        if let Some(end) = trimmed.rfind(']') {
            &trimmed[start..=end]
        } else {
            trimmed
        }
    } else {
        trimmed
    };

    let extractions: Vec<Extraction> = match serde_json::from_str(json_text) {
        Ok(e) => e,
        Err(e) => {
            warn!(
                error = %e,
                text_preview = &text[..text.len().min(200)],
                "observer failed to parse extraction JSON"
            );
            return Ok(Vec::new());
        }
    };

    // Filter by confidence threshold.
    let filtered: Vec<Extraction> = extractions
        .into_iter()
        .filter(|e| e.confidence >= MIN_CONFIDENCE)
        .filter(|e| !e.content.is_empty())
        .collect();

    debug!(count = filtered.len(), "observer parsed extractions");
    Ok(filtered)
}

/// Build a simple user message from text for testing.
pub fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: MessageContent::Text(text.to_owned()),
    }
}
