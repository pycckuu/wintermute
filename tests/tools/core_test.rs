//! Tests for `src/tools/core.rs` — core tool implementations and definitions.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use wintermute::agent::policy::RateLimiter;
use wintermute::executor::{
    ExecOptions, ExecResult, Executor, ExecutorError, ExecutorKind, HealthStatus,
};
use wintermute::memory::{Memory, MemoryEngine, MemoryKind, MemorySource, MemoryStatus};
use wintermute::tools::core::{
    core_tool_definitions, execute_command, memory_save, memory_search, validate_save_path,
    web_request,
};

// ---------------------------------------------------------------------------
// Mock executor
// ---------------------------------------------------------------------------

/// A mock executor that returns canned results for testing.
struct MockExecutor {
    result: ExecResult,
    scripts_dir: PathBuf,
    workspace_dir: PathBuf,
}

impl MockExecutor {
    fn new(result: ExecResult) -> Self {
        Self {
            result,
            scripts_dir: PathBuf::from("/tmp/scripts"),
            workspace_dir: PathBuf::from("/tmp/workspace"),
        }
    }
}

#[async_trait]
impl Executor for MockExecutor {
    async fn execute(
        &self,
        _command: &str,
        _opts: ExecOptions,
    ) -> Result<ExecResult, ExecutorError> {
        Ok(self.result.clone())
    }

    async fn health_check(&self) -> Result<HealthStatus, ExecutorError> {
        Ok(HealthStatus::Healthy {
            kind: ExecutorKind::Direct,
            details: "mock".to_owned(),
        })
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
// Memory test helpers
// ---------------------------------------------------------------------------

async fn setup_memory_engine() -> MemoryEngine {
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

// ---------------------------------------------------------------------------
// Tool definitions tests
// ---------------------------------------------------------------------------

#[test]
fn core_tool_definitions_returns_eight_tools() {
    let defs = core_tool_definitions();
    assert_eq!(defs.len(), 8, "should have exactly 8 core tools");
}

#[test]
fn core_tool_definitions_have_correct_names() {
    let defs = core_tool_definitions();
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();

    assert!(names.contains(&"execute_command"));
    assert!(names.contains(&"web_fetch"));
    assert!(names.contains(&"web_request"));
    assert!(names.contains(&"browser"));
    assert!(names.contains(&"memory_search"));
    assert!(names.contains(&"memory_save"));
    assert!(names.contains(&"send_telegram"));
    assert!(names.contains(&"create_tool"));
}

#[test]
fn core_tool_definitions_have_valid_json_schemas() {
    let defs = core_tool_definitions();
    for def in &defs {
        let schema = &def.input_schema;
        assert_eq!(
            schema.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "tool '{}' should have type: object in its schema",
            def.name
        );
    }
}

#[test]
fn core_tool_definitions_have_descriptions() {
    let defs = core_tool_definitions();
    for def in &defs {
        assert!(
            !def.description.is_empty(),
            "tool '{}' should have a non-empty description",
            def.name
        );
    }
}

// ---------------------------------------------------------------------------
// execute_command tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn execute_command_formats_result_correctly() {
    let exec_result = ExecResult {
        exit_code: Some(0),
        stdout: "hello world\n".to_owned(),
        stderr: String::new(),
        timed_out: false,
        duration: Duration::from_millis(50),
    };
    let executor = MockExecutor::new(exec_result);

    let input = json!({"command": "echo hello world"});
    let result = execute_command(&executor, &input).await;

    assert!(result.is_ok());
    let output = result.expect("should succeed");
    assert!(output.contains("Exit code: 0"));
    assert!(output.contains("Timed out: false"));
    assert!(output.contains("hello world"));
}

#[tokio::test]
async fn execute_command_missing_command_field_returns_error() {
    let exec_result = ExecResult {
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
        timed_out: false,
        duration: Duration::from_millis(0),
    };
    let executor = MockExecutor::new(exec_result);

    let input = json!({});
    let result = execute_command(&executor, &input).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn execute_command_shows_timeout() {
    let exec_result = ExecResult {
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        timed_out: true,
        duration: Duration::from_secs(120),
    };
    let executor = MockExecutor::new(exec_result);

    let input = json!({"command": "sleep 999"});
    let result = execute_command(&executor, &input)
        .await
        .expect("should succeed");

    assert!(result.contains("Timed out: true"));
    assert!(result.contains("Exit code: none"));
}

#[tokio::test]
async fn execute_command_rejects_excessive_timeout() {
    let exec_result = ExecResult {
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
        timed_out: false,
        duration: Duration::from_millis(0),
    };
    let executor = MockExecutor::new(exec_result);

    let input = json!({"command": "echo ok", "timeout_secs": 4000});
    let result = execute_command(&executor, &input).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn web_request_rejects_oversized_body_before_network_call() {
    let limiter = RateLimiter::new(60, 10);
    let input = json!({
        "url": "https://example.com/api",
        "method": "POST",
        "body": "x".repeat(120_000),
    });
    let result = web_request(&input, &limiter).await;
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// memory_search tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_search_returns_formatted_json() {
    let engine = setup_memory_engine().await;

    let mem = Memory {
        id: None,
        kind: MemoryKind::Fact,
        content: "Rust is a systems programming language".to_owned(),
        metadata: None,
        status: MemoryStatus::Active,
        source: MemorySource::Agent,
        created_at: None,
        updated_at: None,
    };
    engine.save_memory(mem).await.expect("save should succeed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let input = json!({"query": "Rust programming"});
    let result = memory_search(&engine, &input)
        .await
        .expect("search should succeed");

    // Parse back to verify it's valid JSON.
    let parsed: serde_json::Value =
        serde_json::from_str(&result).expect("result should be valid JSON");
    assert!(parsed.is_array(), "result should be a JSON array");

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// memory_save tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_save_creates_memory_with_correct_kind_and_source() {
    let engine = setup_memory_engine().await;

    let input = json!({
        "content": "The deployment process requires SSH access",
        "kind": "procedure"
    });
    let result = memory_save(&engine, &input)
        .await
        .expect("save should succeed");

    assert!(result.contains("procedure"));

    // Verify it was saved by searching.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let results = engine
        .search("deployment SSH", 10)
        .await
        .expect("search should succeed");

    assert!(!results.is_empty(), "saved memory should be searchable");
    let saved = &results[0];
    assert_eq!(saved.kind, MemoryKind::Procedure);
    assert_eq!(saved.source, MemorySource::Agent);
    assert_eq!(saved.status, MemoryStatus::Active);

    engine.shutdown().await;
}

#[tokio::test]
async fn memory_save_rejects_invalid_kind() {
    let engine = setup_memory_engine().await;

    let input = json!({
        "content": "test content",
        "kind": "invalid_kind"
    });
    let result = memory_save(&engine, &input).await;
    assert!(result.is_err());

    engine.shutdown().await;
}

#[tokio::test]
async fn memory_save_rejects_missing_content() {
    let engine = setup_memory_engine().await;

    let input = json!({"kind": "fact"});
    let result = memory_save(&engine, &input).await;
    assert!(result.is_err());

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// web_fetch save_to path validation tests
// ---------------------------------------------------------------------------

#[test]
fn validate_save_path_accepts_workspace_path() {
    assert!(validate_save_path("/workspace/file.bin").is_ok());
}

#[test]
fn validate_save_path_accepts_nested_workspace_path() {
    assert!(validate_save_path("/workspace/sub/dir/file.tar.gz").is_ok());
}

#[test]
fn validate_save_path_rejects_non_workspace_path() {
    assert!(validate_save_path("/tmp/file.bin").is_err());
}

#[test]
fn validate_save_path_rejects_root_path() {
    assert!(validate_save_path("/etc/passwd").is_err());
}

#[test]
fn validate_save_path_rejects_traversal() {
    assert!(validate_save_path("/workspace/../etc/passwd").is_err());
}

#[test]
fn validate_save_path_rejects_relative_path() {
    assert!(validate_save_path("workspace/file.bin").is_err());
}

#[test]
fn web_fetch_definition_includes_save_to_property() {
    let defs = core_tool_definitions();
    let fetch_def = defs
        .iter()
        .find(|d| d.name == "web_fetch")
        .expect("web_fetch should exist");
    let props = fetch_def.input_schema["properties"]
        .as_object()
        .expect("should have properties");
    assert!(
        props.contains_key("save_to"),
        "web_fetch should have save_to property"
    );
}
