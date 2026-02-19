//! Input guard credential scanning tests.

use wintermute::telegram::input_guard::{scan_message, GuardAction};

#[test]
fn clean_message_returns_pass() {
    let result = scan_message("hello world, how are you?", &[]);
    assert!(matches!(result, GuardAction::Pass(text) if text == "hello world, how are you?"));
}

#[test]
fn message_that_is_a_credential_returns_blocked() {
    // A message that IS a credential (more than half matched)
    let result = scan_message("ghp_abcdefghijklmnopqrstuvwxyz1234", &[]);
    assert!(matches!(result, GuardAction::Blocked));
}

#[test]
fn message_containing_credential_returns_redacted() {
    let result = scan_message(
        "Please use token ghp_abcdefghijklmnopqrstuvwxyz1234 for access to the repo and do other things with it",
        &[],
    );
    assert!(
        matches!(result, GuardAction::Redacted(ref text) if !text.contains("ghp_")),
        "got: {result:?}"
    );
}

#[test]
fn multiple_credentials_are_all_redacted() {
    let msg = "Keys: sk-ant-ABCDEF12345678901234 and ghp_abcdefghijklmnopqrstuvwxyz1234 are important. Keep them safe. This is a long message to ensure the credentials do not dominate.";
    let result = scan_message(msg, &[]);
    match result {
        GuardAction::Redacted(text) => {
            assert!(!text.contains("sk-ant-"));
            assert!(!text.contains("ghp_"));
            assert!(text.contains("[REDACTED]"));
        }
        other => panic!("expected Redacted, got {other:?}"),
    }
}

#[test]
fn known_secret_exact_match_is_detected() {
    let secrets = vec!["my-super-secret-key-value-12345".to_owned()];
    let result = scan_message(
        "Use my-super-secret-key-value-12345 in the config. This is a long message with context around it to avoid blocking.",
        &secrets,
    );
    assert!(
        matches!(result, GuardAction::Redacted(ref text) if !text.contains("my-super-secret-key-value-12345")),
        "got: {result:?}"
    );
}

#[test]
fn empty_message_returns_pass() {
    let result = scan_message("", &[]);
    assert!(matches!(result, GuardAction::Pass(text) if text.is_empty()));
}
