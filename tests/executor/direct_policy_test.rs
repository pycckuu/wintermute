//! Strict executor policy tests.

use std::path::PathBuf;

use wintermute::executor::direct::DirectExecutor;
use wintermute::executor::{ExecOptions, Executor, ExecutorError, ExecutorKind};

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
    let status = match health {
        Ok(status) => status,
        Err(err) => panic!("health should be available: {err}"),
    };

    assert!(status.is_healthy);
    assert_eq!(status.kind, ExecutorKind::Direct);
}
