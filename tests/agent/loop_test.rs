//! Tests for the agent loop session events and reasoning cycle.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

use wintermute::agent::approval::ApprovalManager;
use wintermute::agent::budget::{DailyBudget, SessionBudget};
use wintermute::agent::policy::PolicyContext;
use wintermute::agent::r#loop::{SessionConfig, SessionEvent};
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

/// Create the in-memory SQLite tables needed by MemoryEngine.
async fn setup_memory_db(db: &sqlx::SqlitePool) {
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
            .execute(db)
            .await
            .expect("failed to create table");
    }
}

// ---------------------------------------------------------------------------
// SessionEvent tests
// ---------------------------------------------------------------------------

#[test]
fn session_event_user_message_can_be_constructed() {
    let event = SessionEvent::UserMessage("hello".to_owned());
    assert!(matches!(event, SessionEvent::UserMessage(ref s) if s == "hello"));
}

#[test]
fn session_event_shutdown_can_be_constructed() {
    let event = SessionEvent::Shutdown;
    assert!(matches!(event, SessionEvent::Shutdown));
}

#[test]
fn session_event_approval_resolved_can_be_constructed() {
    use wintermute::agent::approval::ApprovalResult;
    let result = ApprovalResult::Approved {
        session_id: "s1".to_owned(),
        tool_name: "web_fetch".to_owned(),
        tool_input: "{}".to_owned(),
    };
    let event = SessionEvent::ApprovalResolved(result);
    assert!(matches!(event, SessionEvent::ApprovalResolved(_)));
}

// ---------------------------------------------------------------------------
// estimate_request_tokens tests
// ---------------------------------------------------------------------------

#[test]
fn estimate_messages_tokens_returns_reasonable_value() {
    use wintermute::agent::context::estimate_messages_tokens;
    use wintermute::providers::{Message, MessageContent, Role};

    let messages = vec![Message {
        role: Role::User,
        content: MessageContent::Text("Hello, world!".to_owned()), // 13 chars ~ 3-4 tokens
    }];

    let estimate = estimate_messages_tokens(&messages);
    // 13 chars / 4 chars_per_token = ~3-4 tokens
    assert!(estimate >= 2, "estimate too low: {estimate}");
    assert!(estimate <= 10, "estimate too high: {estimate}");
}

#[test]
fn estimate_messages_tokens_empty_is_zero() {
    use wintermute::agent::context::estimate_messages_tokens;

    let estimate = estimate_messages_tokens(&[]);
    assert_eq!(estimate, 0);
}

// ---------------------------------------------------------------------------
// run_session shutdown test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_session_completes_on_shutdown() {
    // Verify that sending Shutdown causes run_session to complete
    // without needing an LLM call.

    let db = sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("failed to create in-memory db");
    setup_memory_db(&db).await;

    let memory = Arc::new(
        MemoryEngine::new(db, None)
            .await
            .expect("failed to create memory engine"),
    );

    let (telegram_tx, mut telegram_rx) = mpsc::channel::<TelegramOutbound>(16);

    // Drain the telegram channel so nothing blocks
    tokio::spawn(async move { while telegram_rx.recv().await.is_some() {} });

    let daily = Arc::new(DailyBudget::new(1_000_000));
    let budget = SessionBudget::new(Arc::clone(&daily), BudgetConfig::default());
    let approval_manager = Arc::new(ApprovalManager::new());

    let policy_context = PolicyContext {
        allowed_domains: vec![],
        blocked_domains: vec![],
        always_approve_domains: vec![],
        executor_kind: ExecutorKind::Direct,
    };

    let (event_tx, event_rx) = mpsc::channel::<SessionEvent>(16);

    // Use "ollama" provider which doesn't need API credentials.
    // Session will shut down before any LLM call is attempted.
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
    let fetch_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 30));
    let request_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 10));
    let tool_router = Arc::new(wintermute::tools::ToolRouter::new(
        executor,
        redactor,
        Arc::clone(&memory),
        registry,
        None,
        fetch_limiter,
        request_limiter,
        policy_context.clone(),
    ));

    let cfg = SessionConfig {
        session_id: "test-session".to_owned(),
        user_id: 12345,
        router,
        tool_router,
        memory,
        budget,
        approval_manager,
        policy_context,
        telegram_tx,
        config: Arc::new(make_config()),
        agent_config: Arc::new(make_agent_config()),
    };

    // Spawn the session task
    let handle = tokio::spawn(wintermute::agent::r#loop::run_session(cfg, event_rx));

    // Send shutdown immediately
    event_tx
        .send(SessionEvent::Shutdown)
        .await
        .expect("failed to send shutdown");

    // Session should complete promptly
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    assert!(result.is_ok(), "session did not shut down in time");
}

#[tokio::test]
async fn run_session_completes_on_channel_close() {
    // Verify that dropping the event sender causes run_session to complete.

    let db = sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("failed to create in-memory db");
    setup_memory_db(&db).await;

    let memory = Arc::new(
        MemoryEngine::new(db, None)
            .await
            .expect("failed to create memory engine"),
    );

    let (telegram_tx, mut telegram_rx) = mpsc::channel::<TelegramOutbound>(16);
    tokio::spawn(async move { while telegram_rx.recv().await.is_some() {} });

    let daily = Arc::new(DailyBudget::new(1_000_000));
    let budget = SessionBudget::new(Arc::clone(&daily), BudgetConfig::default());
    let approval_manager = Arc::new(ApprovalManager::new());

    let policy_context = PolicyContext {
        allowed_domains: vec![],
        blocked_domains: vec![],
        always_approve_domains: vec![],
        executor_kind: ExecutorKind::Direct,
    };

    let (event_tx, event_rx) = mpsc::channel::<SessionEvent>(16);

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
    let fetch_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 30));
    let request_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 10));
    let tool_router = Arc::new(wintermute::tools::ToolRouter::new(
        executor,
        redactor,
        Arc::clone(&memory),
        registry,
        None,
        fetch_limiter,
        request_limiter,
        policy_context.clone(),
    ));

    let cfg = SessionConfig {
        session_id: "test-session-close".to_owned(),
        user_id: 12345,
        router,
        tool_router,
        memory,
        budget,
        approval_manager,
        policy_context,
        telegram_tx,
        config: Arc::new(make_config()),
        agent_config: Arc::new(make_agent_config()),
    };

    let handle = tokio::spawn(wintermute::agent::r#loop::run_session(cfg, event_rx));

    // Drop the sender to close the channel
    drop(event_tx);

    let result = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    assert!(
        result.is_ok(),
        "session did not shut down after channel close"
    );
}

// ---------------------------------------------------------------------------
// Test executor (minimal)
// ---------------------------------------------------------------------------

/// Minimal executor for tests that never actually runs commands.
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
            details: "test executor".to_owned(),
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
