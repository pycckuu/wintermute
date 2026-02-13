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

impl ExtractedMetadata {
    /// Check if extracted metadata suggests this tool could be useful (spec 7, fast path).
    ///
    /// Maps the detected intent to tool domain prefixes. Returns `false` when
    /// `intent` is `None` (no tools needed for greetings/casual chat). Unknown
    /// intents conservatively return `true` so new intents aren't silently swallowed.
    pub fn could_use(&self, tool: &str) -> bool {
        let intent = match &self.intent {
            Some(i) => i.as_str(),
            None => return false,
        };

        // Extract tool domain (everything before first dot, or entire string).
        let domain = if let Some(dot_pos) = tool.find('.') {
            &tool[..dot_pos]
        } else {
            tool
        };

        match intent {
            "email_check" | "email_reply" | "email_send" => {
                domain == "email" || domain == "message"
            }
            "scheduling" => domain == "calendar",
            "github_check" => domain == "github",
            "web_browse" => domain == "browser" || domain == "http",
            "admin_config" => domain == "admin",
            _ => true, // unknown intent — conservative, assume tools needed
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_with_intent(intent: Option<&str>) -> ExtractedMetadata {
        ExtractedMetadata {
            intent: intent.map(str::to_owned),
            entities: vec![],
            dates_mentioned: vec![],
            extra: serde_json::Value::Null,
        }
    }

    #[test]
    fn test_could_use_none_intent_returns_false() {
        let meta = meta_with_intent(None);
        assert!(!meta.could_use("email.list"));
        assert!(!meta.could_use("calendar.freebusy"));
        assert!(!meta.could_use("admin.status"));
    }

    /// Helper: assert that metadata with given intent matches expected tools
    /// and rejects unexpected ones.
    fn assert_intent_matches(intent: &str, should_match: &[&str], should_not_match: &[&str]) {
        let meta = meta_with_intent(Some(intent));
        for tool in should_match {
            assert!(
                meta.could_use(tool),
                "intent '{intent}' should match {tool}"
            );
        }
        for tool in should_not_match {
            assert!(
                !meta.could_use(tool),
                "intent '{intent}' should NOT match {tool}"
            );
        }
    }

    #[test]
    fn test_could_use_email_intents_match_email_domain() {
        for intent in &["email_check", "email_reply", "email_send"] {
            assert_intent_matches(
                intent,
                &["email.list", "email.read", "message.send"],
                &["calendar.freebusy", "github.list_prs"],
            );
        }
    }

    #[test]
    fn test_could_use_scheduling_matches_calendar() {
        assert_intent_matches(
            "scheduling",
            &["calendar.freebusy", "calendar.list_events"],
            &["email.list"],
        );
    }

    #[test]
    fn test_could_use_github_matches_github() {
        assert_intent_matches(
            "github_check",
            &["github.list_prs", "github.get_issue"],
            &["email.list"],
        );
    }

    #[test]
    fn test_could_use_web_browse_matches_browser_and_http() {
        assert_intent_matches(
            "web_browse",
            &["browser.open_session", "http.request"],
            &["email.list"],
        );
    }

    #[test]
    fn test_could_use_admin_matches_admin() {
        assert_intent_matches(
            "admin_config",
            &["admin.list_integrations", "admin.system_status"],
            &["email.list"],
        );
    }

    #[test]
    fn test_could_use_unknown_intent_returns_true() {
        let meta = meta_with_intent(Some("unknown_future_intent"));
        assert!(meta.could_use("email.list"));
        assert!(meta.could_use("anything.at_all"));
    }

    #[test]
    fn test_could_use_bare_domain_without_dot() {
        let meta = meta_with_intent(Some("email_check"));
        // Tool name without a dot — domain is the whole string.
        assert!(meta.could_use("email"));
        assert!(!meta.could_use("calendar"));
    }
}
