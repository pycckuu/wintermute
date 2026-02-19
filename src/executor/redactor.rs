//! Secret redaction chokepoint for tool and executor outputs.

use regex::Regex;

/// Canonical replacement marker for redacted content.
pub const REDACTION_MARKER: &str = "[REDACTED]";

/// Redacts known secret values and token-like patterns from output text.
#[derive(Debug, Clone)]
pub struct Redactor {
    exact_secrets: Vec<String>,
    patterns: Vec<Regex>,
}

impl Redactor {
    /// Create a redactor from known secret values.
    pub fn new(exact_secrets: Vec<String>) -> Self {
        let patterns = default_patterns();
        Self {
            exact_secrets,
            patterns,
        }
    }

    /// Redact exact known secrets and known secret patterns.
    pub fn redact(&self, text: &str) -> String {
        let mut sanitized = text.to_owned();
        for secret in &self.exact_secrets {
            if !secret.is_empty() {
                sanitized = sanitized.replace(secret, REDACTION_MARKER);
            }
        }
        for pattern in &self.patterns {
            sanitized = pattern
                .replace_all(&sanitized, REDACTION_MARKER)
                .to_string();
        }
        sanitized
    }
}

fn default_patterns() -> Vec<Regex> {
    let patterns = [
        r"sk-ant-[A-Za-z0-9_\-]{10,}",
        r"sk-[A-Za-z0-9]{32,}",
        r"ghp_[A-Za-z0-9]{20,}",
        r"glpat-[A-Za-z0-9_\-]{16,}",
        r"xoxb-[A-Za-z0-9\-]{20,}",
    ];

    patterns
        .iter()
        .filter_map(|pattern| Regex::new(pattern).ok())
        .collect()
}
