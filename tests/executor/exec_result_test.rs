//! ExecResult method tests.

use std::time::Duration;

use wintermute::executor::ExecResult;

#[test]
fn success_when_exit_zero_no_timeout() {
    let result = ExecResult {
        exit_code: Some(0),
        stdout: "ok".to_owned(),
        stderr: String::new(),
        timed_out: false,
        duration: Duration::from_millis(100),
    };
    assert!(result.success());
}

#[test]
fn not_success_when_nonzero_exit() {
    let result = ExecResult {
        exit_code: Some(1),
        stdout: String::new(),
        stderr: "err".to_owned(),
        timed_out: false,
        duration: Duration::from_millis(50),
    };
    assert!(!result.success());
}

#[test]
fn not_success_when_timed_out() {
    let result = ExecResult {
        exit_code: Some(0),
        stdout: "partial".to_owned(),
        stderr: String::new(),
        timed_out: true,
        duration: Duration::from_secs(120),
    };
    assert!(!result.success());
}

#[test]
fn not_success_when_no_exit_code() {
    let result = ExecResult {
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        timed_out: false,
        duration: Duration::from_millis(10),
    };
    assert!(!result.success());
}

#[test]
fn output_combines_stdout_and_stderr() {
    let result = ExecResult {
        exit_code: Some(0),
        stdout: "out".to_owned(),
        stderr: "err".to_owned(),
        timed_out: false,
        duration: Duration::from_millis(10),
    };
    assert_eq!(result.output(), "out\nerr");
}

#[test]
fn output_returns_just_stdout_when_no_stderr() {
    let result = ExecResult {
        exit_code: Some(0),
        stdout: "out".to_owned(),
        stderr: String::new(),
        timed_out: false,
        duration: Duration::from_millis(10),
    };
    assert_eq!(result.output(), "out");
}

#[test]
fn output_returns_just_stderr_when_no_stdout() {
    let result = ExecResult {
        exit_code: Some(1),
        stdout: String::new(),
        stderr: "err".to_owned(),
        timed_out: false,
        duration: Duration::from_millis(10),
    };
    assert_eq!(result.output(), "err");
}
