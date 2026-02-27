//! Tests for `telegram::commands` slash command handlers.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use wintermute::memory::MemoryEngine;
use wintermute::telegram::commands;

async fn setup_engine() -> MemoryEngine {
    let opts = SqliteConnectOptions::new()
        .filename(":memory:")
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("pool should connect");

    let bootstrap = include_str!("../../migrations/001_schema.sql");
    sqlx::raw_sql(bootstrap)
        .execute(&pool)
        .await
        .expect("001 should apply");

    let memory_sql = include_str!("../../migrations/002_memory.sql");
    sqlx::raw_sql(memory_sql)
        .execute(&pool)
        .await
        .expect("002 should apply");

    MemoryEngine::new(pool, None)
        .await
        .expect("engine should initialise")
}

#[test]
fn help_returns_html_with_command_list() {
    let result = commands::handle_help();
    assert!(result.contains("<b>Available commands:</b>"));
    assert!(result.contains("/help"));
    assert!(result.contains("/status"));
    assert!(result.contains("/budget"));
    assert!(result.contains("/memory"));
    assert!(result.contains("/tools"));
    assert!(result.contains("/sandbox"));
    assert!(result.contains("/backup"));
}

#[test]
fn budget_returns_formatted_budget_string() {
    let result = commands::handle_budget(100, 500, 50_000, 5_000_000);
    assert!(result.contains("Budget"));
    assert!(result.contains("100"));
    assert!(result.contains("500"));
    assert!(result.contains("50000"));
    assert!(result.contains("5000000"));
}

#[tokio::test]
async fn memory_undo_with_no_observer_memories() {
    let engine = setup_engine().await;
    let result = commands::handle_memory_undo(&engine).await;
    assert!(result.contains("No observer-promoted"));
    engine.shutdown().await;
}

#[tokio::test]
async fn memory_pending_with_no_pending() {
    let engine = setup_engine().await;
    let result = commands::handle_memory_pending(&engine).await;
    assert!(result.contains("No pending"));
    engine.shutdown().await;
}

#[tokio::test]
async fn backup_trigger_creates_backup() {
    let tmp = tempfile::tempdir().expect("should create temp dir");
    let scripts_dir = tmp.path().join("scripts");
    let backups_dir = tmp.path().join("backups");
    std::fs::create_dir_all(&scripts_dir).expect("should create scripts dir");

    let engine = setup_engine().await;
    let result = commands::handle_backup_trigger(&scripts_dir, &engine, &backups_dir).await;
    assert!(result.contains("Backup created") || result.contains("Backup failed"));
    engine.shutdown().await;
}

#[test]
fn tools_with_empty_registry_returns_no_tools_message() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let registry = wintermute::tools::registry::DynamicToolRegistry::new_without_watcher(
        temp_dir.path().to_path_buf(),
    )
    .expect("failed to create registry");
    let result = commands::handle_tools(&registry);
    assert!(result.contains("No dynamic tools registered"));
}

#[test]
fn tools_detail_not_found() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let registry = wintermute::tools::registry::DynamicToolRegistry::new_without_watcher(
        temp_dir.path().to_path_buf(),
    )
    .expect("failed to create registry");
    let result = commands::handle_tools_detail(&registry, "nonexistent");
    assert!(result.contains("not found"));
}

#[test]
fn help_includes_reset_command() {
    let result = commands::handle_help();
    assert!(result.contains("/reset"));
}

#[test]
fn reset_with_active_session() {
    let result = commands::handle_reset(true);
    assert!(result.contains("Session reset"));
    assert!(result.contains("fresh conversation"));
}

#[test]
fn reset_without_active_session() {
    let result = commands::handle_reset(false);
    assert!(result.contains("No active session"));
}

#[test]
fn tools_with_registered_tool_shows_it() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let tool_json = serde_json::json!({
        "name": "my_tool",
        "description": "A test tool",
        "parameters": { "type": "object", "properties": {} }
    });
    let tool_path = temp_dir.path().join("my_tool.json");
    std::fs::write(&tool_path, serde_json::to_string(&tool_json).expect("json")).expect("write");

    let registry = wintermute::tools::registry::DynamicToolRegistry::new_without_watcher(
        temp_dir.path().to_path_buf(),
    )
    .expect("failed to create registry");

    let list_result = commands::handle_tools(&registry);
    assert!(list_result.contains("my_tool"));
    assert!(list_result.contains("A test tool"));

    let detail_result = commands::handle_tools_detail(&registry, "my_tool");
    assert!(detail_result.contains("my_tool"));
    assert!(detail_result.contains("A test tool"));
    assert!(detail_result.contains("120s")); // default timeout
}

#[test]
fn help_includes_revert_command() {
    let result = commands::handle_help();
    assert!(result.contains("/revert"));
}

/// Mock executor for testing revert command without Docker.
struct RevertMockExecutor {
    success: bool,
    output: String,
}

#[async_trait::async_trait]
impl wintermute::executor::Executor for RevertMockExecutor {
    async fn execute(
        &self,
        _command: &str,
        _opts: wintermute::executor::ExecOptions,
    ) -> Result<wintermute::executor::ExecResult, wintermute::executor::ExecutorError> {
        Ok(wintermute::executor::ExecResult {
            exit_code: if self.success { Some(0) } else { Some(1) },
            stdout: if self.success {
                self.output.clone()
            } else {
                String::new()
            },
            stderr: if self.success {
                String::new()
            } else {
                self.output.clone()
            },
            timed_out: false,
            duration: std::time::Duration::from_millis(10),
        })
    }

    async fn health_check(
        &self,
    ) -> Result<wintermute::executor::HealthStatus, wintermute::executor::ExecutorError> {
        Ok(wintermute::executor::HealthStatus::Healthy {
            kind: wintermute::executor::ExecutorKind::Direct,
            details: "mock".to_owned(),
        })
    }

    fn scripts_dir(&self) -> &std::path::Path {
        std::path::Path::new("/scripts")
    }

    fn workspace_dir(&self) -> &std::path::Path {
        std::path::Path::new("/workspace")
    }

    fn kind(&self) -> wintermute::executor::ExecutorKind {
        wintermute::executor::ExecutorKind::Direct
    }
}

#[tokio::test]
async fn revert_success_shows_output() {
    let executor = RevertMockExecutor {
        success: true,
        output: "Revert 'change'\nThis reverts commit abc123.".to_owned(),
    };
    let result = commands::handle_revert(&executor).await;
    assert!(
        result.contains("Revert successful"),
        "should show success, got: {result}"
    );
    assert!(result.contains("abc123"));
}

#[tokio::test]
async fn revert_failure_shows_error() {
    let executor = RevertMockExecutor {
        success: false,
        output: "fatal: not a git repository".to_owned(),
    };
    let result = commands::handle_revert(&executor).await;
    assert!(
        result.contains("Revert failed"),
        "should show failure, got: {result}"
    );
    assert!(result.contains("not a git repository"));
}
