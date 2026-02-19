//! Tests for redact_result method.

use std::time::Duration;

use wintermute::executor::docker::RawExecResult;
use wintermute::executor::redactor::Redactor;

#[test]
fn redact_result_sanitizes_stdout_and_stderr() {
    let redactor = Redactor::new(vec!["secret123".to_owned()]);
    let raw = RawExecResult {
        exit_code: Some(0),
        stdout: "output with secret123 value".to_owned(),
        stderr: "error with secret123 value".to_owned(),
        timed_out: false,
        duration: Duration::from_millis(50),
    };
    let result = redactor.redact_result(raw);
    assert!(!result.stdout.contains("secret123"));
    assert!(!result.stderr.contains("secret123"));
    assert!(result.stdout.contains("[REDACTED]"));
    assert!(result.stderr.contains("[REDACTED]"));
    assert_eq!(result.exit_code, Some(0));
    assert!(!result.timed_out);
    assert_eq!(result.duration, Duration::from_millis(50));
}

#[test]
fn redact_result_preserves_metadata() {
    let redactor = Redactor::new(vec![]);
    let raw = RawExecResult {
        exit_code: None,
        stdout: "clean output".to_owned(),
        stderr: String::new(),
        timed_out: true,
        duration: Duration::from_secs(120),
    };
    let result = redactor.redact_result(raw);
    assert_eq!(result.exit_code, None);
    assert!(result.timed_out);
    assert_eq!(result.duration, Duration::from_secs(120));
    assert_eq!(result.stdout, "clean output");
}
