//! Outbound message privacy scanner.
//!
//! Scans composed messages for private information that should not be shared.
//! High-severity matches block the message; low-severity matches are logged.

use serde::{Deserialize, Serialize};

use super::brief::{Constraint, TaskBrief};

/// A detected privacy concern in an outbound message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionWarning {
    /// Category of the detected privacy issue.
    pub category: String,
    /// The specific term or pattern that was found.
    pub found: String,
    /// How severe this finding is.
    pub severity: Severity,
}

/// Severity of a redaction warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    /// Informational, logged but not blocking.
    Low,
    /// Blocks the message from being sent.
    High,
}

/// Terms that reveal the agent is an AI system.
const AGENT_IDENTITY_TERMS: &[&str] = &[
    "wintermute",
    "ai agent",
    "automated",
    "i am an ai",
    "artificial intelligence",
    "language model",
    "llm",
];

/// Terms that reveal internal system architecture.
const SYSTEM_ARCHITECTURE_TERMS: &[&str] = &[
    "docker",
    "sandbox",
    "execute_command",
    "tool_call",
    "memory_search",
    "sqlite",
];

/// Terms that suggest the agent is quoting its memory store.
const MEMORY_REFERENCE_TERMS: &[&str] = &[
    "i recall",
    "my records show",
    "according to my memory",
    "i remember from my",
];

/// Terms related to health information.
const HEALTH_INFO_TERMS: &[&str] = &[
    "diagnosis",
    "medication",
    "medical condition",
    "health condition",
    "prescription",
    "treatment plan",
];

/// Scans outbound messages for private information leaks.
pub struct OutboundRedactor {
    custom_terms: Vec<String>,
}

impl OutboundRedactor {
    /// Create a redactor with optional custom blocked terms.
    pub fn new(custom_terms: Vec<String>) -> Self {
        Self { custom_terms }
    }

    /// Scan a composed message for privacy violations.
    ///
    /// Returns a list of warnings. High-severity warnings should block the message.
    pub fn scan(&self, message: &str, brief: &TaskBrief) -> Vec<RedactionWarning> {
        let mut warnings = Vec::new();
        let lower = message.to_lowercase();

        // Agent identity patterns (HIGH severity)
        for term in AGENT_IDENTITY_TERMS {
            if lower.contains(term) {
                warnings.push(RedactionWarning {
                    category: "agent_identity".to_owned(),
                    found: (*term).to_owned(),
                    severity: Severity::High,
                });
            }
        }

        // System architecture patterns (HIGH severity)
        for term in SYSTEM_ARCHITECTURE_TERMS {
            if lower.contains(term) {
                warnings.push(RedactionWarning {
                    category: "system_architecture".to_owned(),
                    found: (*term).to_owned(),
                    severity: Severity::High,
                });
            }
        }

        // Budget ceiling detection (HIGH severity)
        for constraint in &brief.constraints {
            if let Constraint::Budget {
                ceiling, currency, ..
            } = constraint
            {
                let ceiling_str = format!("{ceiling}");
                #[allow(clippy::cast_possible_truncation)]
                let ceiling_int = format!("{}", *ceiling as i64);
                if message.contains(&ceiling_str) || message.contains(&ceiling_int) {
                    warnings.push(RedactionWarning {
                        category: "budget_ceiling".to_owned(),
                        found: format!("{currency}{ceiling}"),
                        severity: Severity::High,
                    });
                }
            }
        }

        // Memory references (LOW severity)
        for term in MEMORY_REFERENCE_TERMS {
            if lower.contains(term) {
                warnings.push(RedactionWarning {
                    category: "memory_reference".to_owned(),
                    found: (*term).to_owned(),
                    severity: Severity::Low,
                });
            }
        }

        // Health info patterns (HIGH severity) -- only if not in shareable_info
        for term in HEALTH_INFO_TERMS {
            if lower.contains(term)
                && !brief
                    .shareable_info
                    .iter()
                    .any(|s| s.to_lowercase().contains(term))
            {
                warnings.push(RedactionWarning {
                    category: "health_info".to_owned(),
                    found: (*term).to_owned(),
                    severity: Severity::High,
                });
            }
        }

        // Custom terms from config
        for term in &self.custom_terms {
            let term_lower = term.to_lowercase();
            if lower.contains(&term_lower) {
                warnings.push(RedactionWarning {
                    category: "custom_term".to_owned(),
                    found: term.clone(),
                    severity: Severity::High,
                });
            }
        }

        warnings
    }

    /// Returns true if any warning has High severity.
    pub fn has_blocking_warnings(warnings: &[RedactionWarning]) -> bool {
        warnings.iter().any(|w| w.severity == Severity::High)
    }
}
