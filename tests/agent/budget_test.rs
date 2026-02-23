//! Budget tracking tests.

use std::sync::Arc;

use wintermute::agent::budget::{BudgetError, BudgetScope, BudgetStatus, DailyBudget, SessionBudget};
use wintermute::config::BudgetConfig;

fn test_config(session: u64, daily: u64, tool_calls: u32) -> BudgetConfig {
    BudgetConfig {
        max_tokens_per_session: session,
        max_tokens_per_day: daily,
        max_tool_calls_per_turn: tool_calls,
        max_dynamic_tools_per_turn: 20,
    }
}

#[test]
fn daily_budget_limit_returns_configured_value() {
    let daily = DailyBudget::new(42_000);
    assert_eq!(daily.limit(), 42_000);
}

#[test]
fn session_budget_allows_when_under_limit() {
    let daily = Arc::new(DailyBudget::new(100_000));
    let budget = SessionBudget::new(daily, test_config(10_000, 100_000, 20));

    assert!(budget.check_budget(5_000).is_ok());
}

#[test]
fn session_budget_rejects_at_limit() {
    let daily = Arc::new(DailyBudget::new(100_000));
    let budget = SessionBudget::new(daily, test_config(10_000, 100_000, 20));

    budget.record_usage(5_000, 4_000);
    // 9_000 used, trying to add 2_000 => 11_000 > 10_000
    let result = budget.check_budget(2_000);
    assert!(result.is_err());
    assert!(matches!(
        result,
        Err(BudgetError::SessionLimitExceeded { .. })
    ));
}

#[test]
fn daily_budget_resets_on_day_change() {
    let daily = DailyBudget::new(1_000);
    daily.record(500);
    assert_eq!(daily.used(), 500);

    // Force a day change by manipulating reset_day
    // We set it to a different value so the next check triggers a reset.
    daily
        .reset_day
        .store(0, std::sync::atomic::Ordering::Relaxed);
    // After the reset, used should be 0.
    assert_eq!(daily.used(), 0);
}

#[test]
fn record_usage_increments_both_session_and_daily() {
    let daily = Arc::new(DailyBudget::new(100_000));
    let budget = SessionBudget::new(daily, test_config(50_000, 100_000, 20));

    budget.record_usage(1_000, 2_000);
    assert_eq!(budget.session_used(), 3_000);
    assert_eq!(budget.daily_used(), 3_000);

    budget.record_usage(500, 500);
    assert_eq!(budget.session_used(), 4_000);
    assert_eq!(budget.daily_used(), 4_000);
}

#[test]
fn check_tool_calls_enforces_per_turn_limit() {
    let daily = Arc::new(DailyBudget::new(100_000));
    let budget = SessionBudget::new(daily, test_config(50_000, 100_000, 5));

    assert!(budget.check_tool_calls(3).is_ok());
    assert!(budget.check_tool_calls(5).is_ok());

    let result = budget.check_tool_calls(6);
    assert!(result.is_err());
    assert!(matches!(
        result,
        Err(BudgetError::ToolCallsExceeded { count: 6, limit: 5 })
    ));
}

#[tokio::test]
async fn concurrent_increments_are_consistent() {
    let daily = Arc::new(DailyBudget::new(10_000_000));
    let config = test_config(10_000_000, 10_000_000, 100);

    let budget = Arc::new(SessionBudget::new(daily, config));

    let mut handles = Vec::new();
    for _ in 0..100 {
        let b = Arc::clone(&budget);
        handles.push(tokio::spawn(async move {
            b.record_usage(100, 0);
        }));
    }

    for h in handles {
        h.await.ok();
    }

    assert_eq!(budget.session_used(), 10_000);
    assert_eq!(budget.daily_used(), 10_000);
}

// ---------------------------------------------------------------------------
// Budget status + warning threshold tests
// ---------------------------------------------------------------------------

#[test]
fn budget_status_ok_when_under_thresholds() {
    let daily = Arc::new(DailyBudget::new(100_000));
    let budget = SessionBudget::new(daily, test_config(10_000, 100_000, 20));

    budget.record_usage(3_000, 0); // 30%
    let (status, _scope) = budget.budget_status();
    assert_eq!(status, BudgetStatus::Ok);
}

#[test]
fn budget_status_warning_at_70_percent() {
    let daily = Arc::new(DailyBudget::new(100_000));
    let budget = SessionBudget::new(daily, test_config(10_000, 100_000, 20));

    budget.record_usage(7_000, 0); // 70%
    let (status, scope) = budget.budget_status();
    assert_eq!(
        status,
        BudgetStatus::Warning {
            level: "elevated",
            percent: 70,
        }
    );
    assert_eq!(scope, BudgetScope::Session);
}

#[test]
fn budget_status_warning_at_85_percent() {
    let daily = Arc::new(DailyBudget::new(100_000));
    let budget = SessionBudget::new(daily, test_config(10_000, 100_000, 20));

    budget.record_usage(4_000, 4_500); // 85%
    let (status, _scope) = budget.budget_status();
    assert_eq!(
        status,
        BudgetStatus::Warning {
            level: "high",
            percent: 85,
        }
    );
}

#[test]
fn budget_status_warning_at_95_percent() {
    let daily = Arc::new(DailyBudget::new(100_000));
    let budget = SessionBudget::new(daily, test_config(10_000, 100_000, 20));

    budget.record_usage(5_000, 4_500); // 95%
    let (status, _scope) = budget.budget_status();
    assert_eq!(
        status,
        BudgetStatus::Warning {
            level: "critical",
            percent: 95,
        }
    );
}

#[test]
fn budget_status_exhausted_at_100_percent() {
    let daily = Arc::new(DailyBudget::new(100_000));
    let budget = SessionBudget::new(daily, test_config(10_000, 100_000, 20));

    budget.record_usage(5_000, 5_000); // 100%
    let (status, scope) = budget.budget_status();
    assert_eq!(status, BudgetStatus::Exhausted);
    assert_eq!(scope, BudgetScope::Session);
}

#[test]
fn budget_status_daily_triggers_when_worse_than_session() {
    // Daily limit is tight, session limit is generous
    let daily = Arc::new(DailyBudget::new(10_000));
    let budget = SessionBudget::new(daily, test_config(100_000, 10_000, 20));

    budget.record_usage(3_500, 3_600); // session: 7.1%, daily: 71%
    let (status, scope) = budget.budget_status();
    assert_eq!(
        status,
        BudgetStatus::Warning {
            level: "elevated",
            percent: 70,
        }
    );
    assert_eq!(scope, BudgetScope::Daily);
}

#[test]
fn session_percent_accuracy() {
    let daily = Arc::new(DailyBudget::new(100_000));
    let budget = SessionBudget::new(daily, test_config(10_000, 100_000, 20));

    assert_eq!(budget.session_percent(), 0);
    budget.record_usage(2_500, 0);
    assert_eq!(budget.session_percent(), 25);
    budget.record_usage(5_000, 0);
    assert_eq!(budget.session_percent(), 75);
}

#[test]
fn daily_percent_accuracy() {
    let daily = Arc::new(DailyBudget::new(1_000_000));
    let budget = SessionBudget::new(daily, test_config(500_000, 1_000_000, 20));

    budget.record_usage(500_000, 0);
    assert_eq!(budget.daily_percent(), 50);
}
