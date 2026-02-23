//! Tests for the SessionRouter.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

use wintermute::agent::approval::ApprovalManager;
use wintermute::agent::budget::DailyBudget;
use wintermute::agent::policy::{PolicyContext, RateLimiter};
use wintermute::agent::SessionRouter;
use wintermute::agent::TelegramOutbound;
use wintermute::config::{
    AgentConfig, BudgetConfig, ChannelsConfig, Config, EgressConfig, HeartbeatConfig,
    LearningConfig, ModelsConfig, PersonalityConfig, PrivacyConfig, SandboxConfig, TelegramConfig,
};
use wintermute::executor::ExecutorKind;
use wintermute::memory::MemoryEngine;
use wintermute::providers::router::ModelRouter;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_config() -> Config {
    Config {
        models: ModelsConfig {
            default: "ollama/llama3".to_owned(),
            roles: std::collections::HashMap::new(),
            skills: std::collections::HashMap::new(),
        },
        channels: ChannelsConfig {
            telegram: TelegramConfig {
                bot_token_env: "TEST_BOT_TOKEN".to_owned(),
                allowed_users: vec![12345],
            },
        },
        sandbox: SandboxConfig::default(),
        budget: BudgetConfig {
            max_tokens_per_session: 100_000,
            max_tokens_per_day: 1_000_000,
            max_tool_calls_per_turn: 20,
            max_dynamic_tools_per_turn: 10,
        },
        egress: EgressConfig::default(),
        privacy: PrivacyConfig::default(),
    }
}

fn make_agent_config() -> AgentConfig {
    AgentConfig {
        personality: PersonalityConfig {
            name: "TestBot".to_owned(),
            soul: "You are a test assistant.".to_owned(),
        },
        heartbeat: HeartbeatConfig::default(),
        learning: LearningConfig::default(),
        scheduled_tasks: vec![],
    }
}

/// Minimal test executor that does nothing.
struct TestExecutor;

#[async_trait]
impl wintermute::executor::Executor for TestExecutor {
    async fn execute(
        &self,
        _command: &str,
        _opts: wintermute::executor::ExecOptions,
    ) -> Result<wintermute::executor::ExecResult, wintermute::executor::ExecutorError> {
        Ok(wintermute::executor::ExecResult {
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            duration: std::time::Duration::from_millis(1),
        })
    }

    async fn health_check(
        &self,
    ) -> Result<wintermute::executor::HealthStatus, wintermute::executor::ExecutorError> {
        Ok(wintermute::executor::HealthStatus::Healthy {
            kind: ExecutorKind::Direct,
            details: "test".to_owned(),
        })
    }

    fn has_network_isolation(&self) -> bool {
        false
    }

    fn scripts_dir(&self) -> &std::path::Path {
        std::path::Path::new("/tmp/wintermute-test-scripts")
    }

    fn workspace_dir(&self) -> &std::path::Path {
        std::path::Path::new("/tmp/wintermute-test-workspace")
    }

    fn kind(&self) -> ExecutorKind {
        ExecutorKind::Direct
    }
}

async fn build_session_router() -> (SessionRouter, mpsc::Receiver<TelegramOutbound>) {
    let db = sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("failed to create in-memory db");

    // Create required tables
    for sql in [
        "CREATE TABLE IF NOT EXISTS memories (
            id INTEGER PRIMARY KEY, kind TEXT NOT NULL, content TEXT NOT NULL,
            metadata TEXT, status TEXT NOT NULL DEFAULT 'active',
            source TEXT NOT NULL DEFAULT 'agent',
            created_at TEXT DEFAULT (datetime('now')),
            updated_at TEXT DEFAULT (datetime('now'))
        )",
        "CREATE TABLE IF NOT EXISTS conversations (
            id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, role TEXT NOT NULL,
            content TEXT NOT NULL, tokens_used INTEGER,
            created_at TEXT DEFAULT (datetime('now'))
        )",
        "CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(content, content=memories, content_rowid=id)",
        "CREATE TABLE IF NOT EXISTS trust_ledger (
            id INTEGER PRIMARY KEY, domain TEXT NOT NULL UNIQUE,
            approved_by TEXT NOT NULL, created_at TEXT DEFAULT (datetime('now'))
        )",
    ] {
        sqlx::query(sql)
            .execute(&db)
            .await
            .expect("failed to create table");
    }

    let memory = Arc::new(
        MemoryEngine::new(db, None)
            .await
            .expect("failed to create memory engine"),
    );

    let creds = wintermute::credentials::Credentials::from_map(BTreeMap::new());
    let models_config = ModelsConfig {
        default: "ollama/llama3".to_owned(),
        roles: std::collections::HashMap::new(),
        skills: std::collections::HashMap::new(),
    };
    let router = Arc::new(
        ModelRouter::from_config(&models_config, &creds).expect("failed to build model router"),
    );

    let executor = Arc::new(TestExecutor);
    let redactor = wintermute::executor::redactor::Redactor::new(vec![]);
    let registry = wintermute::tools::registry::DynamicToolRegistry::new_without_watcher(
        std::path::PathBuf::from("/tmp/wintermute-test-scripts"),
    )
    .expect("failed to create test registry");
    let fetch_limiter = Arc::new(RateLimiter::new(60, 30));
    let request_limiter = Arc::new(RateLimiter::new(60, 10));
    let browser_limiter = Arc::new(RateLimiter::new(60, 60));

    let policy_context = PolicyContext {
        allowed_domains: vec![],
        blocked_domains: vec![],
        always_approve_domains: vec![],
        executor_kind: ExecutorKind::Direct,
    };

    let tool_router = Arc::new(wintermute::tools::ToolRouter::new(
        executor,
        redactor,
        Arc::clone(&memory),
        registry,
        None,
        fetch_limiter,
        request_limiter,
        browser_limiter,
        None,
    ));

    let daily_budget = Arc::new(DailyBudget::new(1_000_000));
    let approval_manager = Arc::new(ApprovalManager::new());
    let (telegram_tx, telegram_rx) = mpsc::channel::<TelegramOutbound>(64);

    let session_router = SessionRouter::new(
        router,
        tool_router,
        memory,
        daily_budget,
        approval_manager,
        policy_context,
        telegram_tx,
        Arc::new(make_config()),
        Arc::new(make_agent_config()),
        None,
    );

    (session_router, telegram_rx)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_router_creates_session_on_first_message() {
    let (router, mut _tg_rx) = build_session_router().await;

    assert_eq!(router.session_count().await, 0);

    // Route a message — should create a session
    let result = router.route_message(12345, "Hello!".to_owned()).await;
    assert!(result.is_ok());

    // Allow the spawned task to start
    tokio::task::yield_now().await;

    assert_eq!(router.session_count().await, 1);
}

#[tokio::test]
async fn session_router_routes_to_existing_session() {
    let (router, mut _tg_rx) = build_session_router().await;

    // First message creates session
    router
        .route_message(12345, "First".to_owned())
        .await
        .expect("first message failed");

    tokio::task::yield_now().await;
    assert_eq!(router.session_count().await, 1);

    // Second message should route to same session (still 1 session)
    router
        .route_message(12345, "Second".to_owned())
        .await
        .expect("second message failed");

    tokio::task::yield_now().await;
    assert_eq!(router.session_count().await, 1);
}

#[tokio::test]
async fn session_router_creates_separate_sessions_per_user() {
    let (router, mut _tg_rx) = build_session_router().await;

    router
        .route_message(111, "Hello from user 111".to_owned())
        .await
        .expect("user 111 message failed");

    router
        .route_message(222, "Hello from user 222".to_owned())
        .await
        .expect("user 222 message failed");

    tokio::task::yield_now().await;
    assert_eq!(router.session_count().await, 2);
}

#[tokio::test]
async fn shutdown_all_clears_sessions() {
    let (router, mut _tg_rx) = build_session_router().await;

    router
        .route_message(12345, "Hello".to_owned())
        .await
        .expect("message failed");

    tokio::task::yield_now().await;
    assert_eq!(router.session_count().await, 1);

    router.shutdown_all().await;

    assert_eq!(router.session_count().await, 0);
}

#[tokio::test]
async fn session_count_reflects_active_sessions() {
    let (router, mut _tg_rx) = build_session_router().await;

    assert_eq!(router.session_count().await, 0);

    router
        .route_message(1, "a".to_owned())
        .await
        .expect("msg failed");
    router
        .route_message(2, "b".to_owned())
        .await
        .expect("msg failed");
    router
        .route_message(3, "c".to_owned())
        .await
        .expect("msg failed");

    tokio::task::yield_now().await;
    assert_eq!(router.session_count().await, 3);

    router.shutdown_all().await;
    assert_eq!(router.session_count().await, 0);
}
