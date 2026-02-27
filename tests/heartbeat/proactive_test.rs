//! Tests for `heartbeat::proactive` behavior checks.

/// Verify the proactive module is accessible and the function signature is correct.
#[test]
fn proactive_module_accessible() {
    // The function requires a ModelRouter + DailyBudget which are hard to construct
    // in unit tests. We verify the module compiles and is accessible.
    let _ = wintermute::heartbeat::proactive::run_proactive_check;
}
