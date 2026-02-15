//! Cold credential detection — intercepts token pastes before pipeline (session-amnesia F1).
//!
//! When a message matches a known credential pattern AND no active admin flow
//! exists, the kernel auto-stores the credential and notifies the owner.
//! The raw token NEVER reaches the Synthesizer (Invariant B).

/// Known credential pattern for cold detection (session-amnesia F1).
///
/// Each pattern uses simple prefix matching + character validation,
/// avoiding a regex dependency.
struct CredentialPattern {
    /// Service name (e.g. "notion", "github").
    service: &'static str,
    /// Prefix the token must start with (e.g. "ntn_").
    prefix: &'static str,
    /// Minimum total length including prefix.
    min_len: usize,
    /// Vault reference key for storing the credential.
    vault_ref: &'static str,
    /// Character validator for the part after the prefix.
    validator: fn(char) -> bool,
}

/// Alphanumeric characters only.
fn is_alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

/// Alphanumeric plus hyphens (used by Slack tokens).
fn is_alnum_or_hyphen(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

/// Known credential patterns (session-amnesia F1, spec 8.5).
const KNOWN_PATTERNS: &[CredentialPattern] = &[
    CredentialPattern {
        service: "notion",
        prefix: "ntn_",
        min_len: 44,
        vault_ref: "vault:notion_api_token",
        validator: is_alnum,
    },
    CredentialPattern {
        service: "slack",
        prefix: "xoxb-",
        min_len: 20,
        vault_ref: "vault:slack_bot_token",
        validator: is_alnum_or_hyphen,
    },
    CredentialPattern {
        service: "github",
        prefix: "ghp_",
        min_len: 40,
        vault_ref: "vault:github_api_token",
        validator: is_alnum,
    },
    CredentialPattern {
        service: "openai",
        prefix: "sk-",
        min_len: 20,
        vault_ref: "vault:openai_api_key",
        validator: is_alnum_or_hyphen,
    },
];

/// Detect a credential pattern in a raw message (session-amnesia F1).
///
/// Returns `(service_name, vault_ref)` if the entire message (trimmed)
/// matches a known credential pattern. Only matches when the ENTIRE
/// trimmed message is the token (not embedded in prose).
pub fn detect_credential(text: &str) -> Option<(&'static str, &'static str)> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    for pattern in KNOWN_PATTERNS {
        if trimmed.len() >= pattern.min_len
            && trimmed.starts_with(pattern.prefix)
            && trimmed[pattern.prefix.len()..]
                .chars()
                .all(pattern.validator)
        {
            return Some((pattern.service, pattern.vault_ref));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_notion_credential() {
        // ntn_ + 40 alphanumeric chars = 44 total.
        let token = "ntn_abcdefghijklmnopqrstuvwxyz1234567890ABCD";
        let result = detect_credential(token);
        assert!(result.is_some(), "should detect notion credential");
        let (service, vault_ref) = result.expect("checked");
        assert_eq!(service, "notion");
        assert_eq!(vault_ref, "vault:notion_api_token");
    }

    #[test]
    fn test_detect_github_credential() {
        // ghp_ + 36 alphanumeric chars = 40 total.
        let token = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";
        let result = detect_credential(token);
        assert!(result.is_some(), "should detect github credential");
        let (service, _) = result.expect("checked");
        assert_eq!(service, "github");
    }

    #[test]
    fn test_detect_slack_credential() {
        // xoxb- with hyphens and alphanumeric.
        let token = "xoxb-123456789-987654321-abcdefGHIJKL";
        let result = detect_credential(token);
        assert!(result.is_some(), "should detect slack credential");
        let (service, _) = result.expect("checked");
        assert_eq!(service, "slack");
    }

    #[test]
    fn test_detect_openai_credential() {
        // sk- with alphanumeric and hyphens.
        let token = "sk-proj-abcdefghijklmnopqrstuvwxyz1234567890ABCDEFGHIJKLMNOP";
        let result = detect_credential(token);
        assert!(result.is_some(), "should detect openai credential");
        let (service, _) = result.expect("checked");
        assert_eq!(service, "openai");
    }

    #[test]
    fn test_no_match_for_regular_text() {
        assert!(detect_credential("hello world").is_none());
        assert!(detect_credential("check my email").is_none());
        assert!(detect_credential("ntn_short").is_none()); // too short
        assert!(detect_credential("").is_none());
        assert!(detect_credential("   ").is_none());
    }

    #[test]
    fn test_no_match_embedded_in_prose() {
        // Token embedded in text should NOT match (entire message must be the token).
        let text = "my token is ghp_abcdefghijklmnopqrstuvwxyz1234567890 please save it";
        assert!(detect_credential(text).is_none());
    }

    #[test]
    fn test_trims_whitespace() {
        let token = "  ghp_abcdefghijklmnopqrstuvwxyz1234567890  ";
        let result = detect_credential(token);
        assert!(result.is_some(), "should detect after trimming whitespace");
    }
}
