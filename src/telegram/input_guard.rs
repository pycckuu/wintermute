//! Inbound credential scanning for user messages.
//!
//! Scans incoming Telegram messages for API keys, tokens, and other credentials
//! before they enter the agent pipeline. Messages that are mostly credentials
//! are blocked entirely; messages containing partial credentials are redacted.

use crate::executor::redactor::{default_credential_patterns, REDACTION_MARKER};

/// Action to take after scanning a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardAction {
    /// Message is clean, pass through unchanged.
    Pass(String),
    /// Message contained credentials that have been replaced with markers.
    Redacted(String),
    /// Message is predominantly credentials and should be blocked.
    Blocked,
}

/// Scan a message for credentials and decide the appropriate action.
///
/// Checks `known_secrets` for exact matches first, then applies regex
/// patterns from the shared credential pattern set. If more than half
/// the message characters are matched, the message is blocked entirely.
pub fn scan_message(message: &str, known_secrets: &[String]) -> GuardAction {
    if message.is_empty() {
        return GuardAction::Pass(message.to_owned());
    }

    let patterns = default_credential_patterns();
    let mut redacted = message.to_owned();
    let mut total_matched_chars: usize = 0;

    // Step 1: Check exact secret matches first
    for secret in known_secrets {
        if !secret.is_empty() && redacted.contains(secret.as_str()) {
            let match_count = redacted.matches(secret.as_str()).count();
            total_matched_chars =
                total_matched_chars.saturating_add(secret.len().saturating_mul(match_count));
            redacted = redacted.replace(secret.as_str(), REDACTION_MARKER);
        }
    }

    // Step 2: Check regex pattern matches
    for pattern in &patterns {
        for mat in pattern.find_iter(&redacted.clone()) {
            // Only count characters that are not already redacted
            if mat.as_str() != REDACTION_MARKER {
                total_matched_chars = total_matched_chars.saturating_add(mat.len());
            }
        }
        redacted = pattern.replace_all(&redacted, REDACTION_MARKER).to_string();
    }

    // Step 3: Decide action based on matched character ratio
    if total_matched_chars > message.len() / 2 {
        return GuardAction::Blocked;
    }

    if redacted != message {
        return GuardAction::Redacted(redacted);
    }

    GuardAction::Pass(message.to_owned())
}
