//! Structured extractors — deterministic parsers for Phase 0 (spec 6.10).
//!
//! Extractors output typed fields, NOT free text. They serve two purposes:
//! 1. Feed structured metadata to the Planner without exposing raw content
//! 2. Downgrade taint from Raw to Extracted

pub mod message;

use serde::{Deserialize, Serialize};

/// Extracted metadata from Phase 0 processing (spec 6.10, 7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedMetadata {
    /// Detected intent (e.g., "email_check", "scheduling").
    pub intent: Option<String>,
    /// Typed entities extracted from the message.
    pub entities: Vec<ExtractedEntity>,
    /// Date/time references found in the message.
    pub dates_mentioned: Vec<String>,
    /// Additional structured fields.
    #[serde(default)]
    pub extra: serde_json::Value,
}

/// A typed entity extracted from message content (spec 6.10).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedEntity {
    /// Entity type (e.g., "person", "email_id", "service").
    pub kind: String,
    /// Extracted value.
    pub value: String,
}

/// Trait for structured extractors (spec 6.10).
///
/// Extractors are deterministic (or tightly constrained) parsers that
/// output typed fields. They NEVER produce free text.
pub trait Extractor: Send + Sync {
    /// Extractor identifier for taint tracking.
    fn name(&self) -> &str;
    /// Extract structured metadata from raw text.
    fn extract(&self, text: &str) -> ExtractedMetadata;
}
