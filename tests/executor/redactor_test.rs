//! Output redaction behavior tests.

use wintermute::executor::redactor::{Redactor, REDACTION_MARKER};

#[test]
fn redactor_hides_exact_and_pattern_secrets() {
    let redactor = Redactor::new(vec!["top-secret-value".to_owned()]);
    let input = "secret=top-secret-value token=ghp_abcdefghijklmnopqrstuvwxyz1234";
    let output = redactor.redact(input);

    assert!(!output.contains("top-secret-value"));
    assert!(!output.contains("ghp_abcdefghijklmnopqrstuvwxyz1234"));
    assert!(output.contains(REDACTION_MARKER));
}
