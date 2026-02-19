//! Strict executor policy tests.

use std::path::PathBuf;

use wintermute::executor::direct::DirectExecutor;
use wintermute::executor::{ExecOptions, Executor, ExecutorError, ExecutorKind, HealthStatus};

#[tokio::test]
async fn direct_executor_is_maintenance_only() {
    let executor = DirectExecutor::new(
        PathBuf::from("/tmp/scripts"),
        PathBuf::from("/tmp/workspace"),
    );
    let result = executor.execute("echo hello", ExecOptions::default()).await;

    match result {
        Err(ExecutorError::Forbidden(_)) => {}
        Err(other) => panic!("expected forbidden error, got: {other}"),
        Ok(_) => panic!("direct executor must not execute commands"),
    }
}

#[tokio::test]
async fn direct_executor_reports_kind_and_health() {
    let executor = DirectExecutor::new(
        PathBuf::from("/tmp/scripts"),
        PathBuf::from("/tmp/workspace"),
    );
    let health = executor.health_check().await;
    assert!(health.is_ok());
    let status = health.expect("health should be available");

    // Direct executor reports Degraded (maintenance-only, no sandbox).
    match &status {
        HealthStatus::Degraded { kind, .. } => {
            assert_eq!(*kind, ExecutorKind::Direct);
        }
        other => panic!("expected Degraded, got: {other:?}"),
    }
    assert!(!status.is_healthy());
}
