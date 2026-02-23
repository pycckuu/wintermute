//! Tests for `src/heartbeat/scheduler.rs` — cron evaluation and task dispatch.

use chrono::Utc;

use wintermute::config::ScheduledTaskConfig;
use wintermute::heartbeat::scheduler::{due_tasks, SchedulerState};

fn test_task(name: &str, cron: &str) -> ScheduledTaskConfig {
    ScheduledTaskConfig {
        name: name.to_owned(),
        cron: cron.to_owned(),
        builtin: Some("backup".to_owned()),
        tool: None,
        budget_tokens: None,
        notify: false,
        enabled: true,
    }
}

#[test]
fn due_tasks_returns_matching_cron() {
    let tasks = vec![
        // Every minute.
        test_task("every_minute", "0 * * * * *"),
    ];
    let state = SchedulerState::new();

    // A time that's definitely past at least one cron trigger.
    let now = Utc::now();
    let due = due_tasks(&tasks, &state, now);

    assert_eq!(due.len(), 1);
    assert_eq!(due[0].name, "every_minute");
}

#[test]
fn disabled_tasks_are_skipped() {
    let mut task = test_task("disabled", "0 * * * * *");
    task.enabled = false;

    let tasks = vec![task];
    let state = SchedulerState::new();
    let now = Utc::now();

    let due = due_tasks(&tasks, &state, now);
    assert!(due.is_empty());
}

#[test]
fn task_not_due_if_recently_run() {
    // Task runs every hour.
    let tasks = vec![test_task("hourly", "0 0 * * * *")];

    let mut state = SchedulerState::new();
    // Mark as run just now — should not be due again immediately.
    let now = Utc::now();
    state.record_run("hourly", now);

    let due = due_tasks(&tasks, &state, now);
    assert!(due.is_empty(), "task should not be due right after running");
}

#[test]
fn task_due_after_interval_passes() {
    // Task runs every minute.
    let tasks = vec![test_task("minutely", "0 * * * * *")];

    let mut state = SchedulerState::new();
    // Mark as run 2 minutes ago.
    let two_minutes_ago = Utc::now() - chrono::Duration::minutes(2);
    state.record_run("minutely", two_minutes_ago);

    let now = Utc::now();
    let due = due_tasks(&tasks, &state, now);
    assert_eq!(due.len(), 1);
}

#[test]
fn invalid_cron_expression_is_skipped() {
    let tasks = vec![test_task("bad_cron", "not a cron expression")];
    let state = SchedulerState::new();
    let now = Utc::now();

    let due = due_tasks(&tasks, &state, now);
    assert!(due.is_empty(), "invalid cron should be skipped");
}

#[test]
fn scheduler_state_records_and_retrieves() {
    let mut state = SchedulerState::new();

    assert!(state.last_run_for("task1").is_none());

    let now = Utc::now();
    state.record_run("task1", now);

    let recorded = state
        .last_run_for("task1")
        .expect("should have been recorded");
    assert_eq!(*recorded, now);
}

#[test]
fn multiple_tasks_with_different_schedules() {
    let tasks = vec![
        test_task("every_sec", "* * * * * *"), // every second
        test_task("yearly", "0 0 0 1 1 *"),    // once a year (Jan 1 midnight)
    ];

    let state = SchedulerState::new();
    let now = Utc::now();

    let due = due_tasks(&tasks, &state, now);
    // "every_sec" should be due; "yearly" might or might not be depending on date
    assert!(
        due.iter().any(|t| t.name == "every_sec"),
        "every-second task should be due"
    );
}
