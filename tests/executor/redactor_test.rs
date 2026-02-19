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

#[test]
fn redactor_hides_anthropic_key_pattern() {
    let redactor = Redactor::new(vec![]);
    let input = "key=sk-ant-ABCDEF12345678901234";
    let output = redactor.redact(input);
    assert!(!output.contains("sk-ant-"));
    assert!(output.contains(REDACTION_MARKER));
}

#[test]
fn redactor_hides_openai_key_pattern() {
    let redactor = Redactor::new(vec![]);
    let input = "key=sk-AAAABBBBCCCCDDDDEEEEFFFFGGGGHHHHIIIIJJJJ";
    let output = redactor.redact(input);
    assert!(!output.contains("sk-AAAA"));
    assert!(output.contains(REDACTION_MARKER));
}

#[test]
fn redactor_hides_gitlab_pat_pattern() {
    let redactor = Redactor::new(vec![]);
    let input = "token=glpat-ABCDEFGHIJKLMNOP";
    let output = redactor.redact(input);
    assert!(!output.contains("glpat-"));
    assert!(output.contains(REDACTION_MARKER));
}

#[test]
fn redactor_preserves_clean_text() {
    let redactor = Redactor::new(vec![]);
    let input = "just a normal log line";
    let output = redactor.redact(input);
    assert_eq!(output, input);
}

#[test]
fn redactor_handles_empty_secrets() {
    let redactor = Redactor::new(vec!["".to_owned(), "  ".to_owned()]);
    let input = "safe text";
    let output = redactor.redact(input);
    assert_eq!(output, input);
}

#[test]
fn redactor_handles_empty_input() {
    let redactor = Redactor::new(vec!["secret".to_owned()]);
    let output = redactor.redact("");
    assert_eq!(output, "");
}

#[test]
fn redaction_marker_value() {
    assert_eq!(REDACTION_MARKER, "[REDACTED]");
}
