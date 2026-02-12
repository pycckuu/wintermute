#![allow(missing_docs)]

//! PFAR v2 — Privacy-First Agent Runtime (spec 1).
//!
//! Single Rust binary that receives events from adapters, enforces
//! mandatory access control via the Policy Engine, and orchestrates
//! the Plan-Then-Execute pipeline.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use pfar::adapters::telegram::{AdapterToKernel, KernelToAdapter, TelegramAdapter, TelegramConfig};
use pfar::kernel::approval::ApprovalQueue;
use pfar::kernel::audit::AuditLogger;
use pfar::kernel::egress::EgressValidator;
use pfar::kernel::executor::PlanExecutor;
use pfar::kernel::inference::InferenceProxy;
use pfar::kernel::pipeline::Pipeline;
use pfar::kernel::policy::PolicyEngine;
use pfar::kernel::router::EventRouter;
use pfar::kernel::session::SessionStore;
use pfar::kernel::template::{InferenceConfig, TaskTemplate, TemplateRegistry};
use pfar::kernel::vault::InMemoryVault;
use pfar::tools::calendar::CalendarTool;
use pfar::tools::email::EmailTool;
use pfar::tools::ToolRegistry;
use pfar::types::PrincipalClass;

/// Channel buffer size for adapter <-> kernel communication.
const CHANNEL_BUFFER_SIZE: usize = 100;

/// Default Ollama URL for local inference.
const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";

/// Default Telegram owner ID from spec 18.1.
const DEFAULT_OWNER_ID: &str = "415494855";

/// Default audit log path.
const DEFAULT_AUDIT_LOG_PATH: &str = "/tmp/pfar-audit.jsonl";

/// Default approval timeout in seconds (spec 18.1).
const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 300;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing (spec 14.5).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("PFAR v2 starting");

    // Initialize kernel components (spec 5.1).
    let policy = Arc::new(PolicyEngine::with_defaults());

    // Load templates -- templates use the best available provider (spec 18.2).
    // Future phases will load from ~/.pfar/templates/ directory.
    let owner_inference = resolve_owner_inference_config();
    let templates = Arc::new(create_default_templates(&owner_inference));

    // Initialize vault (in-memory for Phase 2, spec 6.4).
    let vault: Arc<dyn pfar::kernel::vault::SecretStore> = Arc::new(InMemoryVault::default());

    // Initialize inference proxy with available providers (spec 6.3, 11.1).
    let ollama_url =
        std::env::var("PFAR_OLLAMA_URL").unwrap_or_else(|_| DEFAULT_OLLAMA_URL.to_string());
    let inference = Arc::new(build_inference_proxy(&ollama_url));

    // Initialize tool registry with Phase 2 tools (spec 6.11).
    let tools = Arc::new(create_tool_registry());

    // Initialize session store (spec 9.1).
    let sessions = Arc::new(RwLock::new(SessionStore::new()));

    // Initialize audit logger (spec 6.7).
    let audit_path =
        std::env::var("PFAR_AUDIT_LOG").unwrap_or_else(|_| DEFAULT_AUDIT_LOG_PATH.to_string());
    let audit = Arc::new(AuditLogger::new(&audit_path).context("failed to create audit logger")?);

    // Initialize approval queue (spec 6.6).
    let _approval_queue = Arc::new(tokio::sync::Mutex::new(ApprovalQueue::new(
        Duration::from_secs(DEFAULT_APPROVAL_TIMEOUT_SECS),
    )));

    // Build plan executor (spec 7, Phase 2).
    let executor = PlanExecutor::new(
        Arc::clone(&policy),
        Arc::clone(&tools),
        Arc::clone(&vault),
        Arc::clone(&audit),
    );

    // Build egress validator (spec 10.8).
    let egress = EgressValidator::new(Arc::clone(&policy), Arc::clone(&audit));

    // Build pipeline orchestrator (spec 7).
    let pipeline = Pipeline::new(
        Arc::clone(&policy),
        Arc::clone(&inference),
        executor,
        Arc::clone(&sessions),
        egress,
        Arc::clone(&tools),
        Arc::clone(&audit),
    );

    // Build event router (spec 6.1).
    let router = EventRouter::new(
        Arc::clone(&policy),
        Arc::clone(&templates),
        Arc::clone(&audit),
    );

    // Check if Telegram adapter is configured (spec 6.9).
    let bot_token = std::env::var("PFAR_TELEGRAM_BOT_TOKEN").ok();
    let owner_id =
        std::env::var("PFAR_TELEGRAM_OWNER_ID").unwrap_or_else(|_| DEFAULT_OWNER_ID.to_string());

    if let Some(token) = bot_token {
        info!("starting Telegram adapter");
        run_telegram_loop(token, owner_id, router, pipeline, templates).await
    } else {
        info!("no PFAR_TELEGRAM_BOT_TOKEN set -- running in CLI-only mode");
        info!("set PFAR_TELEGRAM_BOT_TOKEN to enable Telegram adapter");
        // Future: fall back to CLI adapter event loop.
        info!("PFAR v2 shutting down (no adapter configured)");
        Ok(())
    }
}

/// Run the Telegram adapter event loop (spec 6.9, 14.1).
///
/// Spawns the adapter as an async task, then processes events from
/// the adapter channel, routing each through the kernel pipeline.
async fn run_telegram_loop(
    token: String,
    owner_id: String,
    router: EventRouter,
    pipeline: Pipeline,
    templates: Arc<TemplateRegistry>,
) -> Result<()> {
    let config = TelegramConfig {
        bot_token: token,
        owner_id,
        poll_timeout_seconds: 30,
    };

    // Create channels for adapter <-> kernel communication.
    let (adapter_tx, mut adapter_rx) = mpsc::channel::<AdapterToKernel>(CHANNEL_BUFFER_SIZE);
    let (kernel_tx, kernel_rx) = mpsc::channel::<KernelToAdapter>(CHANNEL_BUFFER_SIZE);

    // Spawn Telegram adapter as an async task.
    let adapter = TelegramAdapter::new(config);
    tokio::spawn(async move {
        if let Err(e) = adapter.run(adapter_tx, kernel_rx).await {
            error!("Telegram adapter error: {e}");
        }
    });

    info!("PFAR v2 ready -- listening for events");

    // Main event loop: process events from the adapter.
    while let Some(msg) = adapter_rx.recv().await {
        match msg {
            AdapterToKernel::Event(event) => {
                // Extract chat_id from metadata for response routing.
                let chat_id = event
                    .payload
                    .metadata
                    .get("chat_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();

                // Route event through kernel (spec 6.1).
                match router.route_event(*event) {
                    Ok((labeled_event, mut task)) => {
                        let template = templates.get(&task.template_id);

                        if let Some(tmpl) = template {
                            // Run full pipeline (spec 7).
                            match pipeline.run(labeled_event, &mut task, tmpl).await {
                                Ok(output) => {
                                    let _ = kernel_tx
                                        .send(KernelToAdapter::SendMessage {
                                            chat_id,
                                            text: output.response_text,
                                        })
                                        .await;
                                }
                                Err(e) => {
                                    warn!(task_id = %task.task_id, error = %e, "pipeline error");
                                    let _ = kernel_tx
                                        .send(KernelToAdapter::SendMessage {
                                            chat_id,
                                            text: "Sorry, I encountered an error processing your request.".to_owned(),
                                        })
                                        .await;
                                }
                            }
                        } else {
                            warn!("template not found: {}", task.template_id);
                        }
                    }
                    Err(e) => {
                        warn!("routing error: {e}");
                    }
                }
            }
            AdapterToKernel::Heartbeat => {
                debug!("Telegram adapter heartbeat received");
            }
        }
    }

    info!("PFAR v2 shutting down");
    Ok(())
}

/// Build the inference proxy with all available providers (spec 6.3, 11.1).
///
/// Always registers local Ollama. Optionally registers OpenAI, Anthropic,
/// and LM Studio based on environment variables.
fn build_inference_proxy(ollama_url: &str) -> InferenceProxy {
    let mut builder = InferenceProxy::builder(ollama_url);

    if let Ok(key) = std::env::var("PFAR_OPENAI_API_KEY") {
        builder = builder.with_openai("https://api.openai.com", &key);
        info!("OpenAI provider registered");
    }

    if let Ok(key) = std::env::var("PFAR_ANTHROPIC_API_KEY") {
        builder = builder.with_anthropic(&key);
        info!("Anthropic provider registered");
    }

    if let Ok(url) = std::env::var("PFAR_LMSTUDIO_URL") {
        builder = builder.with_lmstudio(&url);
        info!(url = %url, "LM Studio provider registered");
    }

    builder.build()
}

/// Resolve the best available inference config for owner templates (spec 11.1).
///
/// Prefers Anthropic > OpenAI > local, based on which API keys are set.
/// Cloud providers set `owner_acknowledged_cloud_risk: true` since the owner
/// explicitly provided the API key.
fn resolve_owner_inference_config() -> InferenceConfig {
    if std::env::var("PFAR_ANTHROPIC_API_KEY").is_ok() {
        info!("owner templates will use Anthropic provider");
        InferenceConfig {
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            owner_acknowledged_cloud_risk: true,
        }
    } else if std::env::var("PFAR_OPENAI_API_KEY").is_ok() {
        info!("owner templates will use OpenAI provider");
        InferenceConfig {
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            owner_acknowledged_cloud_risk: true,
        }
    } else {
        InferenceConfig {
            provider: "local".to_string(),
            model: "llama3".to_string(),
            owner_acknowledged_cloud_risk: false,
        }
    }
}

/// Create default task templates for Phase 2 (spec 18.2, 18.3).
fn create_default_templates(owner_inference: &InferenceConfig) -> TemplateRegistry {
    let mut registry = TemplateRegistry::new();
    registry.register(owner_telegram_template(owner_inference));
    registry.register(owner_cli_template(owner_inference));
    registry.register(whatsapp_scheduling_template());
    registry
}

fn owner_telegram_template(inference: &InferenceConfig) -> TaskTemplate {
    TaskTemplate {
        template_id: "owner_telegram_general".to_string(),
        triggers: vec!["adapter:telegram:message:owner".to_string()],
        principal_class: PrincipalClass::Owner,
        description: "General assistant for owner via Telegram".to_string(),
        planner_task_description: None,
        allowed_tools: vec![
            "email.list".to_string(),
            "email.read".to_string(),
            "calendar.freebusy".to_string(),
        ],
        denied_tools: vec![],
        max_tool_calls: 15,
        max_tokens_plan: 4000,
        max_tokens_synthesize: 8000,
        output_sinks: vec!["sink:telegram:owner".to_string()],
        data_ceiling: pfar::types::SecurityLabel::Sensitive,
        inference: inference.clone(),
        require_approval_for_writes: false,
    }
}

fn owner_cli_template(inference: &InferenceConfig) -> TaskTemplate {
    TaskTemplate {
        template_id: "owner_cli_general".to_string(),
        triggers: vec!["adapter:cli:message:owner".to_string()],
        principal_class: PrincipalClass::Owner,
        description: "General assistant for owner via CLI".to_string(),
        planner_task_description: None,
        allowed_tools: vec![
            "email.list".to_string(),
            "email.read".to_string(),
            "calendar.freebusy".to_string(),
        ],
        denied_tools: vec![],
        max_tool_calls: 15,
        max_tokens_plan: 4000,
        max_tokens_synthesize: 8000,
        output_sinks: vec!["sink:cli:owner".to_string()],
        data_ceiling: pfar::types::SecurityLabel::Sensitive,
        inference: inference.clone(),
        require_approval_for_writes: false,
    }
}

fn whatsapp_scheduling_template() -> TaskTemplate {
    TaskTemplate {
        template_id: "whatsapp_scheduling".to_string(),
        triggers: vec!["adapter:whatsapp:message:third_party".to_string()],
        principal_class: PrincipalClass::ThirdParty,
        description: "Handle scheduling requests from WhatsApp contacts".to_string(),
        planner_task_description: Some(
            "A contact is requesting to schedule a meeting. \
             Check free/busy and propose available times."
                .to_string(),
        ),
        allowed_tools: vec!["calendar.freebusy".to_string(), "message.reply".to_string()],
        denied_tools: vec!["email.send".to_string()],
        max_tool_calls: 5,
        max_tokens_plan: 2000,
        max_tokens_synthesize: 2000,
        output_sinks: vec!["sink:whatsapp:reply_to_sender".to_string()],
        data_ceiling: pfar::types::SecurityLabel::Internal,
        inference: InferenceConfig {
            provider: "local".to_string(),
            model: "llama3".to_string(),
            owner_acknowledged_cloud_risk: false,
        },
        require_approval_for_writes: false,
    }
}

/// Create tool registry with Phase 2 tools (spec 6.11, 12.2).
fn create_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(CalendarTool::new()));
    registry.register(Box::new(EmailTool::new()));
    registry
}
