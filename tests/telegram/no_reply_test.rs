//! Tests for the [NO_REPLY] response suppression filter.

use wintermute::telegram::is_no_reply;

#[test]
fn exact_no_reply_is_suppressed() {
    assert!(is_no_reply("[NO_REPLY]"));
}

#[test]
fn no_reply_with_trailing_text_is_suppressed() {
    assert!(is_no_reply("[NO_REPLY] some additional text"));
}

#[test]
fn no_reply_with_whitespace_is_suppressed() {
    assert!(is_no_reply("  [NO_REPLY]  "));
}

#[test]
fn normal_message_is_not_suppressed() {
    assert!(!is_no_reply("Hello, how can I help?"));
}

#[test]
fn message_containing_no_reply_mid_text_is_not_suppressed() {
    assert!(!is_no_reply("The answer is not [NO_REPLY] here"));
}

#[test]
fn empty_string_is_not_suppressed() {
    assert!(!is_no_reply(""));
}
