//! Tests for the agent loop session events and reasoning cycle.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
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
use wintermute::providers::{
    CompletionRequest, CompletionResponse, ContentPart, LlmProvider, ProviderError, StopReason,
    UsageStats,
};

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
    let browser_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 60));
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
    let browser_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 60));
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

#[derive(Debug)]
struct OverflowThenSuccessProvider {
    calls: Arc<AtomicU32>,
}

#[async_trait]
impl LlmProvider for OverflowThenSuccessProvider {
    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Err(ProviderError::HttpStatus {
                status: 400,
                body: "context_length_exceeded".to_owned(),
            });
        }

        Ok(CompletionResponse {
            content: vec![ContentPart::Text {
                text: "retry-ok".to_owned(),
            }],
            stop_reason: StopReason::EndTurn,
            usage: UsageStats {
                input_tokens: 10,
                output_tokens: 5,
            },
            model: "test/mock".to_owned(),
        })
    }

    fn supports_tool_calling(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn model_id(&self) -> &str {
        "test/mock"
    }
}

#[derive(Debug)]
struct AlwaysOverflowProvider {
    calls: Arc<AtomicU32>,
}

#[async_trait]
impl LlmProvider for AlwaysOverflowProvider {
    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::HttpStatus {
            status: 400,
            body: "context_length_exceeded".to_owned(),
        })
    }

    fn supports_tool_calling(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn model_id(&self) -> &str {
        "test/mock"
    }
}

#[derive(Debug)]
struct CountingProvider {
    calls: Arc<AtomicU32>,
}

#[async_trait]
impl LlmProvider for CountingProvider {
    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(CompletionResponse {
            content: vec![ContentPart::Text {
                text: "ok".to_owned(),
            }],
            stop_reason: StopReason::EndTurn,
            usage: UsageStats {
                input_tokens: 1,
                output_tokens: 1,
            },
            model: "test/mock".to_owned(),
        })
    }

    fn supports_tool_calling(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn model_id(&self) -> &str {
        "test/mock"
    }
}

#[derive(Debug)]
struct BrowserPolicyFlowProvider {
    calls: Arc<AtomicU32>,
    saw_denied_result: Arc<Mutex<bool>>,
}

#[async_trait]
impl LlmProvider for BrowserPolicyFlowProvider {
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(CompletionResponse {
                content: vec![ContentPart::ToolUse {
                    id: "browser-1".to_owned(),
                    name: "browser".to_owned(),
                    input: json!({
                        "action": "navigate",
                        "url": "https://blocked.example.com/secret"
                    }),
                }],
                stop_reason: StopReason::ToolUse,
                usage: UsageStats {
                    input_tokens: 8,
                    output_tokens: 3,
                },
                model: "test/mock".to_owned(),
            });
        }

        let denied_seen = request.messages.last().is_some_and(|msg| {
            if let wintermute::providers::MessageContent::Parts(parts) = &msg.content {
                return parts.iter().any(|part| match part {
                    ContentPart::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        tool_use_id == "browser-1"
                            && *is_error
                            && content.contains("Denied: domain is blocked")
                    }
                    _ => false,
                });
            }
            false
        });
        if let Ok(mut flag) = self.saw_denied_result.lock() {
            *flag = denied_seen;
        }

        Ok(CompletionResponse {
            content: vec![ContentPart::Text {
                text: if denied_seen {
                    "policy-deny-observed".to_owned()
                } else {
                    "policy-deny-missing".to_owned()
                },
            }],
            stop_reason: StopReason::EndTurn,
            usage: UsageStats {
                input_tokens: 7,
                output_tokens: 2,
            },
            model: "test/mock".to_owned(),
        })
    }

    fn supports_tool_calling(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn model_id(&self) -> &str {
        "test/mock"
    }
}

#[tokio::test]
async fn run_session_retries_on_context_overflow_and_succeeds() {
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
    let daily = Arc::new(DailyBudget::new(1_000_000));
    let budget = SessionBudget::new(Arc::clone(&daily), BudgetConfig::default());
    let approval_manager = Arc::new(ApprovalManager::new());

    let policy_context = PolicyContext {
        allowed_domains: vec![],
        blocked_domains: vec![],
        always_approve_domains: vec![],
        executor_kind: ExecutorKind::Direct,
    };

    let calls = Arc::new(AtomicU32::new(0));
    let provider = Arc::new(OverflowThenSuccessProvider {
        calls: Arc::clone(&calls),
    });
    let router = Arc::new(ModelRouter::for_testing("test/mock".to_owned(), provider));

    let executor = Arc::new(TestExecutor);
    let redactor = wintermute::executor::redactor::Redactor::new(vec![]);
    let registry = wintermute::tools::registry::DynamicToolRegistry::new_without_watcher(
        std::path::PathBuf::from("/tmp/wintermute-test-scripts"),
    )
    .expect("failed to create test registry");
    let fetch_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 30));
    let request_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 10));
    let browser_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 60));
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

    let (event_tx, event_rx) = mpsc::channel::<SessionEvent>(16);
    let cfg = SessionConfig {
        session_id: "overflow-retry-session".to_owned(),
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
    event_tx
        .send(SessionEvent::UserMessage("hello".to_owned()))
        .await
        .expect("failed to send user message");

    let outbound = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(msg) = telegram_rx.recv().await {
                let contains_retry_ok = msg
                    .text
                    .as_deref()
                    .is_some_and(|text| text.contains("retry-ok"));
                if contains_retry_ok {
                    break msg;
                }
            }
        }
    })
    .await;
    assert!(
        outbound.is_ok(),
        "expected assistant success message after retry"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "provider should be called twice (overflow + retry success)"
    );

    event_tx
        .send(SessionEvent::Shutdown)
        .await
        .expect("failed to send shutdown");
    let join = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    assert!(join.is_ok(), "session should stop cleanly");
}

#[tokio::test]
async fn run_session_context_overflow_exhausts_retries_and_sends_error() {
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
    let daily = Arc::new(DailyBudget::new(1_000_000));
    let budget = SessionBudget::new(Arc::clone(&daily), BudgetConfig::default());
    let approval_manager = Arc::new(ApprovalManager::new());

    let policy_context = PolicyContext {
        allowed_domains: vec![],
        blocked_domains: vec![],
        always_approve_domains: vec![],
        executor_kind: ExecutorKind::Direct,
    };

    let calls = Arc::new(AtomicU32::new(0));
    let provider = Arc::new(AlwaysOverflowProvider {
        calls: Arc::clone(&calls),
    });
    let router = Arc::new(ModelRouter::for_testing("test/mock".to_owned(), provider));

    let executor = Arc::new(TestExecutor);
    let redactor = wintermute::executor::redactor::Redactor::new(vec![]);
    let registry = wintermute::tools::registry::DynamicToolRegistry::new_without_watcher(
        std::path::PathBuf::from("/tmp/wintermute-test-scripts"),
    )
    .expect("failed to create test registry");
    let fetch_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 30));
    let request_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 10));
    let browser_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 60));
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

    let (event_tx, event_rx) = mpsc::channel::<SessionEvent>(16);
    let cfg = SessionConfig {
        session_id: "overflow-fail-session".to_owned(),
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
    event_tx
        .send(SessionEvent::UserMessage("hello".to_owned()))
        .await
        .expect("failed to send user message");

    let outbound = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(msg) = telegram_rx.recv().await {
                let has_llm_error = msg
                    .text
                    .as_deref()
                    .is_some_and(|text| text.contains("LLM error"));
                if has_llm_error {
                    break msg;
                }
            }
        }
    })
    .await;
    assert!(
        outbound.is_ok(),
        "expected LLM error after retry exhaustion"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "provider should be called initial + 3 retries"
    );

    event_tx
        .send(SessionEvent::Shutdown)
        .await
        .expect("failed to send shutdown");
    let join = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    assert!(join.is_ok(), "session should stop cleanly");
}

#[tokio::test]
async fn security_invariant_budget_check_happens_before_provider_call() {
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
    let daily = Arc::new(DailyBudget::new(1_000_000));
    let budget = SessionBudget::new(
        Arc::clone(&daily),
        BudgetConfig {
            max_tokens_per_session: 0,
            ..BudgetConfig::default()
        },
    );
    let approval_manager = Arc::new(ApprovalManager::new());

    let policy_context = PolicyContext {
        allowed_domains: vec![],
        blocked_domains: vec![],
        always_approve_domains: vec![],
        executor_kind: ExecutorKind::Direct,
    };

    let calls = Arc::new(AtomicU32::new(0));
    let provider = Arc::new(CountingProvider {
        calls: Arc::clone(&calls),
    });
    let router = Arc::new(ModelRouter::for_testing("test/mock".to_owned(), provider));

    let executor = Arc::new(TestExecutor);
    let redactor = wintermute::executor::redactor::Redactor::new(vec![]);
    let registry = wintermute::tools::registry::DynamicToolRegistry::new_without_watcher(
        std::path::PathBuf::from("/tmp/wintermute-test-scripts"),
    )
    .expect("failed to create test registry");
    let fetch_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 30));
    let request_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 10));
    let browser_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 60));
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

    let (event_tx, event_rx) = mpsc::channel::<SessionEvent>(16);
    let cfg = SessionConfig {
        session_id: "budget-gate-session".to_owned(),
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
    event_tx
        .send(SessionEvent::UserMessage("force budget fail".to_owned()))
        .await
        .expect("failed to send user message");

    let outbound = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(msg) = telegram_rx.recv().await {
                let has_budget_error = msg
                    .text
                    .as_deref()
                    .is_some_and(|text| text.contains("Budget exceeded"));
                if has_budget_error {
                    break msg;
                }
            }
        }
    })
    .await;
    assert!(outbound.is_ok(), "expected budget exceeded message");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "provider must not be called when budget check fails"
    );

    event_tx
        .send(SessionEvent::Shutdown)
        .await
        .expect("failed to send shutdown");
    let join = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    assert!(join.is_ok(), "session should stop cleanly");
}

#[tokio::test]
async fn run_session_applies_browser_policy_to_tool_use_integration() {
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
    let daily = Arc::new(DailyBudget::new(1_000_000));
    let budget = SessionBudget::new(Arc::clone(&daily), BudgetConfig::default());
    let approval_manager = Arc::new(ApprovalManager::new());

    let policy_context = PolicyContext {
        allowed_domains: vec![],
        blocked_domains: vec!["blocked.example.com".to_owned()],
        always_approve_domains: vec![],
        executor_kind: ExecutorKind::Direct,
    };

    let calls = Arc::new(AtomicU32::new(0));
    let saw_denied_result = Arc::new(Mutex::new(false));
    let provider = Arc::new(BrowserPolicyFlowProvider {
        calls: Arc::clone(&calls),
        saw_denied_result: Arc::clone(&saw_denied_result),
    });
    let router = Arc::new(ModelRouter::for_testing("test/mock".to_owned(), provider));

    let executor = Arc::new(TestExecutor);
    let redactor = wintermute::executor::redactor::Redactor::new(vec![]);
    let registry = wintermute::tools::registry::DynamicToolRegistry::new_without_watcher(
        std::path::PathBuf::from("/tmp/wintermute-test-scripts"),
    )
    .expect("failed to create test registry");
    let fetch_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 30));
    let request_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 10));
    let browser_limiter = Arc::new(wintermute::agent::policy::RateLimiter::new(60, 60));
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

    let (event_tx, event_rx) = mpsc::channel::<SessionEvent>(16);
    let cfg = SessionConfig {
        session_id: "browser-policy-loop-session".to_owned(),
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
    event_tx
        .send(SessionEvent::UserMessage("check browser policy".to_owned()))
        .await
        .expect("failed to send user message");

    let outbound = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(msg) = telegram_rx.recv().await {
                let has_marker = msg
                    .text
                    .as_deref()
                    .is_some_and(|text| text.contains("policy-deny-observed"));
                if has_marker {
                    break msg;
                }
            }
        }
    })
    .await;
    assert!(
        outbound.is_ok(),
        "expected follow-up completion confirming denied tool result"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "provider should run initial tool-use turn and one follow-up turn"
    );
    let saw_denied = saw_denied_result.lock().map(|flag| *flag).unwrap_or(false);
    assert!(
        saw_denied,
        "follow-up request should include denied tool result"
    );

    event_tx
        .send(SessionEvent::Shutdown)
        .await
        .expect("failed to send shutdown");
    let join = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    assert!(join.is_ok(), "session should stop cleanly");
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
