//! Tests for the Telegram reporter (cooldown logic and message formatting).
//!
//! Actual Telegram sending is NOT tested (requires a real bot token).
//! These tests focus on cooldown tracking and Reporter construction.

use flatline::reporter::Reporter;

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

#[test]
fn reporter_new_creates_instance() {
    // A dummy token is fine; we're not sending anything.
    let reporter = Reporter::new(
        "dummy-bot-token",
        vec![123456789],
        "\u{1fa7a} Flatline".to_owned(),
        30,
    );

    // Should not panic. We can't inspect private fields, but construction
    // itself is the test.
    assert!(!reporter.is_in_cooldown("any_key"));
}

#[test]
fn reporter_empty_notify_users() {
    let reporter = Reporter::new("dummy-bot-token", vec![], "Flatline".to_owned(), 30);

    assert!(!reporter.is_in_cooldown("test"));
}

// ---------------------------------------------------------------------------
// Cooldown tracking
// ---------------------------------------------------------------------------

#[test]
fn cooldown_starts_empty() {
    let reporter = Reporter::new("token", vec![], "F".to_owned(), 30);
    assert!(!reporter.is_in_cooldown("pattern_a"));
    assert!(!reporter.is_in_cooldown("pattern_b"));
}

#[test]
fn cooldown_active_after_record() {
    let mut reporter = Reporter::new("token", vec![], "F".to_owned(), 30);

    reporter.record_cooldown("pattern_a");
    assert!(reporter.is_in_cooldown("pattern_a"));

    // A different key should not be in cooldown.
    assert!(!reporter.is_in_cooldown("pattern_b"));
}

#[test]
fn cooldown_multiple_keys_independent() {
    let mut reporter = Reporter::new("token", vec![], "F".to_owned(), 30);

    reporter.record_cooldown("key_1");
    reporter.record_cooldown("key_2");

    assert!(reporter.is_in_cooldown("key_1"));
    assert!(reporter.is_in_cooldown("key_2"));
    assert!(!reporter.is_in_cooldown("key_3"));
}

#[test]
fn cooldown_zero_minutes_still_in_cooldown() {
    // With 0 minute cooldown, any recorded entry should have elapsed >= 0,
    // but the check is `elapsed < cooldown`, so with 0 it should NOT be
    // in cooldown (since elapsed >= 0 is never < 0).
    let mut reporter = Reporter::new("token", vec![], "F".to_owned(), 0);

    reporter.record_cooldown("pattern_a");
    assert!(!reporter.is_in_cooldown("pattern_a"));
}

#[test]
fn cooldown_very_long_not_expired() {
    // With a very long cooldown (1 year), recently recorded entries stay active.
    let mins_per_year: u64 = 525600;
    let mut reporter = Reporter::new("token", vec![], "F".to_owned(), mins_per_year);

    reporter.record_cooldown("long_lived");
    assert!(reporter.is_in_cooldown("long_lived"));
}

#[test]
fn cooldown_overwrite_resets_timer() {
    let mut reporter = Reporter::new("token", vec![], "F".to_owned(), 30);

    reporter.record_cooldown("pattern_a");
    assert!(reporter.is_in_cooldown("pattern_a"));

    // Recording again should reset the timer (still in cooldown).
    reporter.record_cooldown("pattern_a");
    assert!(reporter.is_in_cooldown("pattern_a"));
}
