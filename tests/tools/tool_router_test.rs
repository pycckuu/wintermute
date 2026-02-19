//! Tests for `src/tools/mod.rs` — ToolRouter dispatch and redaction.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;

use wintermute::agent::policy::{PolicyContext, RateLimiter};
use wintermute::executor::redactor::Redactor;
use wintermute::executor::{
    ExecOptions, ExecResult, Executor, ExecutorError, ExecutorKind, HealthStatus,
};
use wintermute::tools::registry::DynamicToolRegistry;
use wintermute::tools::ToolRouter;

// ---------------------------------------------------------------------------
// Mock executor for router tests
// ---------------------------------------------------------------------------

struct RouterMockExecutor {
    scripts_dir: PathBuf,
    workspace_dir: PathBuf,
}

impl RouterMockExecutor {
    fn new() -> Self {
        Self {
            scripts_dir: PathBuf::from("/tmp/scripts"),
            workspace_dir: PathBuf::from("/tmp/workspace"),
        }
    }
}

#[async_trait]
impl Executor for RouterMockExecutor {
    async fn execute(
        &self,
        _command: &str,
        _opts: ExecOptions,
    ) -> Result<ExecResult, ExecutorError> {
        Ok(ExecResult {
            exit_code: Some(0),
            stdout: "mock output".to_owned(),
            stderr: String::new(),
            timed_out: false,
            duration: Duration::from_millis(10),
        })
    }

    async fn health_check(&self) -> Result<HealthStatus, ExecutorError> {
        Ok(HealthStatus::Healthy {
            kind: ExecutorKind::Direct,
            details: "mock".to_owned(),
        })
    }

    fn has_network_isolation(&self) -> bool {
        false
    }

    fn scripts_dir(&self) -> &Path {
        &self.scripts_dir
    }

    fn workspace_dir(&self) -> &Path {
        &self.workspace_dir
    }

    fn kind(&self) -> ExecutorKind {
        ExecutorKind::Direct
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn build_router(executor: Arc<dyn Executor>, redactor: Redactor) -> ToolRouter {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let registry =
        DynamicToolRegistry::new_without_watcher(dir.path().to_path_buf()).expect("registry");

    let fetch_limiter = Arc::new(RateLimiter::new(60, 30));
    let request_limiter = Arc::new(RateLimiter::new(60, 10));
    let policy_context = PolicyContext {
        allowed_domains: Vec::new(),
        blocked_domains: Vec::new(),
        always_approve_domains: Vec::new(),
        executor_kind: ExecutorKind::Direct,
    };

    ToolRouter::new(
        executor,
        redactor,
        create_dummy_memory_engine().await,
        registry,
        None,
        fetch_limiter,
        request_limiter,
        policy_context,
    )
}

/// Create a minimal in-memory MemoryEngine for tests that don't use it.
async fn create_dummy_memory_engine() -> Arc<wintermute::memory::MemoryEngine> {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(":memory:")
        .create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
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

    Arc::new(
        wintermute::memory::MemoryEngine::new(pool, None)
            .await
            .expect("engine should initialise"),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_tool_returns_error_result() {
    let executor = Arc::new(RouterMockExecutor::new());
    let redactor = Redactor::new(Vec::new());
    let router = build_router(executor, redactor).await;

    let result = router.execute("totally_unknown", &json!({})).await;
    assert!(result.is_error, "unknown tool should produce error result");
    assert!(
        result.content.contains("Unknown tool"),
        "error should mention unknown tool, got: {}",
        result.content
    );
}

#[tokio::test]
async fn core_tool_dispatch_works() {
    let executor = Arc::new(RouterMockExecutor::new());
    let redactor = Redactor::new(Vec::new());
    let router = build_router(executor, redactor).await;

    let input = json!({"command": "echo test"});
    let result = router.execute("execute_command", &input).await;

    assert!(!result.is_error, "execute_command should succeed");
    assert!(
        result.content.contains("Exit code: 0"),
        "should contain exit code, got: {}",
        result.content
    );
}

#[tokio::test]
async fn output_is_redacted() {
    // Create an executor that returns output containing a known secret.
    struct SecretExecutor {
        scripts_dir: PathBuf,
        workspace_dir: PathBuf,
    }

    #[async_trait]
    impl Executor for SecretExecutor {
        async fn execute(
            &self,
            _command: &str,
            _opts: ExecOptions,
        ) -> Result<ExecResult, ExecutorError> {
            Ok(ExecResult {
                exit_code: Some(0),
                stdout: "The secret is MY_SECRET_TOKEN_12345".to_owned(),
                stderr: String::new(),
                timed_out: false,
                duration: Duration::from_millis(10),
            })
        }

        async fn health_check(&self) -> Result<HealthStatus, ExecutorError> {
            Ok(HealthStatus::Healthy {
                kind: ExecutorKind::Direct,
                details: "mock".to_owned(),
            })
        }

        fn has_network_isolation(&self) -> bool {
            false
        }

        fn scripts_dir(&self) -> &Path {
            &self.scripts_dir
        }

        fn workspace_dir(&self) -> &Path {
            &self.workspace_dir
        }

        fn kind(&self) -> ExecutorKind {
            ExecutorKind::Direct
        }
    }

    let executor = Arc::new(SecretExecutor {
        scripts_dir: PathBuf::from("/tmp/scripts"),
        workspace_dir: PathBuf::from("/tmp/workspace"),
    });

    // Register the secret with the redactor.
    let redactor = Redactor::new(vec!["MY_SECRET_TOKEN_12345".to_owned()]);
    let router = build_router(executor, redactor).await;

    let input = json!({"command": "cat secrets"});
    let result = router.execute("execute_command", &input).await;

    assert!(
        !result.content.contains("MY_SECRET_TOKEN_12345"),
        "secret should be redacted from output"
    );
    assert!(
        result.content.contains("[REDACTED]"),
        "output should contain redaction marker"
    );
}

#[tokio::test]
async fn output_is_redacted_for_memory_search_path() {
    let executor: Arc<dyn Executor> = Arc::new(RouterMockExecutor::new());
    let redactor = Redactor::new(vec!["SENSITIVE_DATA_XYZ".to_owned()]);
    let router = build_router(executor, redactor).await;

    // memory_search returns JSON with results; the redactor should catch any secrets
    // in the output regardless of which dispatch path is used.
    let input = json!({"query": "SENSITIVE_DATA_XYZ", "limit": 5});
    let result = router.execute("memory_search", &input).await;

    assert!(
        !result.content.contains("SENSITIVE_DATA_XYZ"),
        "secret should be redacted from memory_search output"
    );
}

#[tokio::test]
async fn tool_definitions_returns_core_plus_dynamic() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = dir.path().to_path_buf();

    // Add a dynamic tool.
    let schema = json!({
        "name": "custom_tool",
        "description": "A custom dynamic tool",
        "parameters": { "type": "object" }
    });
    std::fs::write(
        path.join("custom_tool.json"),
        serde_json::to_string_pretty(&schema).expect("serialize"),
    )
    .expect("write");

    let registry =
        DynamicToolRegistry::new_without_watcher(path).expect("registry should initialise");

    let executor: Arc<dyn Executor> = Arc::new(RouterMockExecutor::new());
    let redactor = Redactor::new(Vec::new());
    let fetch_limiter = Arc::new(RateLimiter::new(60, 30));
    let request_limiter = Arc::new(RateLimiter::new(60, 10));
    let policy_context = PolicyContext {
        allowed_domains: Vec::new(),
        blocked_domains: Vec::new(),
        always_approve_domains: Vec::new(),
        executor_kind: ExecutorKind::Direct,
    };

    let router = ToolRouter::new(
        executor,
        redactor,
        create_dummy_memory_engine().await,
        registry,
        None,
        fetch_limiter,
        request_limiter,
        policy_context,
    );

    let defs = router.tool_definitions(10);

    // Should have 7 core + 1 dynamic.
    assert_eq!(defs.len(), 8, "should have 7 core + 1 dynamic tool");

    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert!(
        names.contains(&"custom_tool"),
        "should include dynamic tool"
    );
    assert!(
        names.contains(&"execute_command"),
        "should include core tools"
    );
}

#[tokio::test]
async fn tool_definitions_respects_max_dynamic_limit() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = dir.path().to_path_buf();

    // Add 3 dynamic tools.
    for i in 0..3 {
        let name = format!("tool_{i}");
        let schema = json!({
            "name": name,
            "description": format!("Tool {i}"),
            "parameters": { "type": "object" }
        });
        std::fs::write(
            path.join(format!("{name}.json")),
            serde_json::to_string_pretty(&schema).expect("serialize"),
        )
        .expect("write");
    }

    let registry = DynamicToolRegistry::new_without_watcher(path).expect("registry");

    let executor: Arc<dyn Executor> = Arc::new(RouterMockExecutor::new());
    let redactor = Redactor::new(Vec::new());
    let fetch_limiter = Arc::new(RateLimiter::new(60, 30));
    let request_limiter = Arc::new(RateLimiter::new(60, 10));
    let policy_context = PolicyContext {
        allowed_domains: Vec::new(),
        blocked_domains: Vec::new(),
        always_approve_domains: Vec::new(),
        executor_kind: ExecutorKind::Direct,
    };

    let router = ToolRouter::new(
        executor,
        redactor,
        create_dummy_memory_engine().await,
        registry,
        None,
        fetch_limiter,
        request_limiter,
        policy_context,
    );

    // max_dynamic = 1, so total should be 7 core + 1 dynamic = 8.
    let defs = router.tool_definitions(1);
    assert_eq!(
        defs.len(),
        8,
        "should have 7 core + at most 1 dynamic, got {}",
        defs.len()
    );
}
