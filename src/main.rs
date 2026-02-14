#![allow(missing_docs)]

//! PFAR v2 — Privacy-First Agent Runtime (spec 1).
//!
//! Single Rust binary that receives events from adapters, enforces
//! mandatory access control via the Policy Engine, and orchestrates
//! the Plan-Then-Execute pipeline.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use pfar::adapters::telegram::{AdapterToKernel, KernelToAdapter, TelegramAdapter, TelegramConfig};
use pfar::config::{LlmConfig, PfarConfig};
use pfar::kernel::approval::ApprovalQueue;
use pfar::kernel::audit::AuditLogger;
use pfar::kernel::egress::EgressValidator;
use pfar::kernel::executor::PlanExecutor;
use pfar::kernel::inference::InferenceProxy;
use pfar::kernel::journal::TaskJournal;
use pfar::kernel::pipeline::Pipeline;
use pfar::kernel::policy::PolicyEngine;
use pfar::kernel::router::EventRouter;
use pfar::kernel::session::SessionStore;
use pfar::kernel::template::{InferenceConfig, TaskTemplate, TemplateRegistry};
use pfar::kernel::vault::InMemoryVault;
use pfar::tools::admin::AdminTool;
use pfar::tools::calendar::CalendarTool;
use pfar::tools::email::EmailTool;
use pfar::tools::mcp::manager::McpServerManager;
use pfar::tools::memory::MemoryTool;
use pfar::tools::ToolRegistry;
use pfar::types::PrincipalClass;

#[tokio::main]
async fn main() -> Result<()> {
    // Load configuration (spec 18.1).
    // Precedence: env vars > ./config.toml > defaults.
    let config = PfarConfig::load().context("failed to load configuration")?;

    // Initialize tracing (spec 14.5).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&config.kernel.log_level)),
        )
        .init();

    info!("PFAR v2 starting");

    // Initialize kernel components (spec 5.1).
    let policy = Arc::new(PolicyEngine::with_defaults());

    // Load templates — templates use the best available provider (spec 18.2).
    // Future phases will load from ~/.pfar/templates/ directory.
    let owner_inference = resolve_owner_inference_config(&config.llm);
    info!(provider = %owner_inference.provider, model = %owner_inference.model, "owner inference resolved");
    let templates = Arc::new(create_default_templates(&owner_inference));

    // Initialize vault (in-memory for Phase 2, spec 6.4).
    let vault: Arc<dyn pfar::kernel::vault::SecretStore> = Arc::new(InMemoryVault::default());

    // Initialize inference proxy with available providers (spec 6.3, 11.1).
    let inference = Arc::new(build_inference_proxy(&config.llm));

    // Initialize audit logger (spec 6.7).
    let audit = Arc::new(
        AuditLogger::new(&config.paths.audit_log).context("failed to create audit logger")?,
    );

    // Log system startup (feature-persistence-recovery).
    if let Err(e) = audit.log_system_startup(env!("CARGO_PKG_VERSION")) {
        warn!(error = %e, "failed to log startup audit event");
    }

    // Open task journal (feature-persistence-recovery).
    let journal = Arc::new(
        TaskJournal::open(&config.paths.journal_db).context("failed to open task journal")?,
    );
    info!(path = %config.paths.journal_db, "task journal opened");

    // Initialize tool registry with Phase 2 + Phase 3 tools (spec 6.11, 8.2).
    // Two-phase init: base tools first, then AdminTool with MCP manager.
    // Must come after journal creation (MemoryTool needs journal ref).
    let (tools, mcp_manager) = create_tool_registry(
        Arc::clone(&vault),
        Arc::clone(&templates),
        Arc::clone(&journal),
        Arc::clone(&audit),
    );

    // Load persisted session data from journal (spec 9.1, 9.2).
    let sessions = {
        let mut store = SessionStore::new();
        match store.load_from_journal(&journal) {
            Ok(0) => info!("no persisted session data found"),
            Ok(count) => info!(entries = count, "loaded persisted session data"),
            Err(e) => warn!(error = %e, "failed to load session data (non-fatal)"),
        }
        Arc::new(RwLock::new(store))
    };

    // Initialize approval queue (spec 6.6).
    let _approval_queue = Arc::new(tokio::sync::Mutex::new(ApprovalQueue::new(
        Duration::from_secs(config.kernel.approval_timeout_seconds),
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
        Some(Arc::clone(&journal)),
    );

    // Build event router (spec 6.1).
    let router = EventRouter::new(
        Arc::clone(&policy),
        Arc::clone(&templates),
        Arc::clone(&audit),
    );

    // Check if Telegram adapter is configured (spec 6.9).
    if let Some(token) = config.adapter.telegram.bot_token.clone() {
        info!("starting Telegram adapter");
        run_telegram_loop(
            token,
            &config,
            router,
            pipeline,
            templates,
            audit,
            Arc::clone(&journal),
            mcp_manager,
        )
        .await
    } else {
        info!("no Telegram bot token configured -- running in CLI-only mode");
        info!("set PFAR_TELEGRAM_BOT_TOKEN or [adapter.telegram].bot_token to enable");
        // Shut down MCP servers if any were auto-started.
        mcp_manager.shutdown_all().await;
        info!("PFAR v2 shutting down (no adapter configured)");
        Ok(())
    }
}

/// Run the Telegram adapter event loop (spec 6.9, 14.1).
///
/// Spawns the adapter as an async task, then processes events from
/// the adapter channel, routing each through the kernel pipeline.
/// Handles graceful shutdown on SIGINT/SIGTERM (spec 9).
#[allow(clippy::too_many_arguments)]
async fn run_telegram_loop(
    token: String,
    config: &PfarConfig,
    router: EventRouter,
    pipeline: Pipeline,
    templates: Arc<TemplateRegistry>,
    audit: Arc<AuditLogger>,
    journal: Arc<TaskJournal>,
    mcp_manager: Arc<McpServerManager>,
) -> Result<()> {
    let owner_chat_id = config.adapter.telegram.owner_id.clone();

    let tg_config = TelegramConfig {
        bot_token: token,
        owner_id: config.adapter.telegram.owner_id.clone(),
        poll_timeout_seconds: config.adapter.telegram.poll_timeout_seconds,
    };

    // Create channels for adapter <-> kernel communication.
    let (adapter_tx, mut adapter_rx) =
        mpsc::channel::<AdapterToKernel>(config.kernel.channel_buffer_size);
    let (kernel_tx, kernel_rx) =
        mpsc::channel::<KernelToAdapter>(config.kernel.channel_buffer_size);

    // Shutdown flag and active task counter (spec 9).
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let active_tasks = Arc::new(AtomicUsize::new(0));

    // Spawn Telegram adapter with journal for offset persistence.
    let adapter = TelegramAdapter::new(tg_config, Some(journal));
    tokio::spawn(async move {
        if let Err(e) = adapter.run(adapter_tx, kernel_rx).await {
            error!("Telegram adapter error: {e}");
        }
    });

    info!("PFAR v2 ready -- listening for events");

    // Notify owner of restart (persistence-recovery spec §8).
    let _ = kernel_tx
        .send(KernelToAdapter::SendMessage {
            chat_id: owner_chat_id,
            text: "System restarted. If you were waiting on something, just ask again.".to_owned(),
        })
        .await;

    // Main event loop with graceful shutdown support.
    loop {
        tokio::select! {
            msg = adapter_rx.recv() => {
                let Some(msg) = msg else {
                    info!("adapter channel closed");
                    break;
                };

                if shutdown_flag.load(Ordering::Relaxed) {
                    debug!("shutdown in progress, ignoring new event");
                    continue;
                }

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
                                    active_tasks.fetch_add(1, Ordering::Relaxed);
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
                                    active_tasks.fetch_sub(1, Ordering::Relaxed);
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
            _ = tokio::signal::ctrl_c() => {
                info!("received shutdown signal, initiating graceful shutdown");
                shutdown_flag.store(true, Ordering::Relaxed);
                break;
            }
        }
    }

    // Graceful shutdown sequence (spec 9).
    let shutdown_timeout_secs = config.kernel.shutdown_timeout_seconds;

    let pending = active_tasks.load(Ordering::Relaxed);
    if pending > 0 {
        info!(
            pending_tasks = pending,
            timeout_secs = shutdown_timeout_secs,
            "waiting for in-flight tasks"
        );
        let deadline = tokio::time::Instant::now()
            .checked_add(Duration::from_secs(shutdown_timeout_secs))
            .unwrap_or_else(tokio::time::Instant::now);

        while active_tasks.load(Ordering::Relaxed) > 0 {
            if tokio::time::Instant::now() >= deadline {
                let remaining = active_tasks.load(Ordering::Relaxed);
                warn!(
                    remaining_tasks = remaining,
                    "shutdown timeout exceeded, abandoning in-flight tasks"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // Shut down MCP servers (feature-dynamic-integrations).
    mcp_manager.shutdown_all().await;

    // Flush audit log and log shutdown event.
    let final_pending = active_tasks.load(Ordering::Relaxed);
    if let Err(e) = audit.log_system_shutdown(final_pending) {
        warn!(error = %e, "failed to log shutdown audit event");
    }

    // Signal adapter to stop.
    let _ = kernel_tx.send(KernelToAdapter::Shutdown).await;

    info!("PFAR v2 shut down cleanly");
    Ok(())
}

/// Build the inference proxy from config (spec 6.3, 11.1).
///
/// Always registers local Ollama. Optionally registers cloud/local providers
/// based on config.
fn build_inference_proxy(llm: &LlmConfig) -> InferenceProxy {
    let mut builder = InferenceProxy::builder(&llm.local.base_url);

    if let Some(ref openai) = llm.openai {
        builder = builder.with_openai(&openai.base_url, &openai.api_key);
        info!("OpenAI provider registered");
    }

    if let Some(ref anthropic) = llm.anthropic {
        builder = builder.with_anthropic(&anthropic.api_key);
        info!("Anthropic provider registered");
    }

    if let Some(ref lmstudio) = llm.lmstudio {
        builder = builder.with_lmstudio(&lmstudio.base_url);
        info!(url = %lmstudio.base_url, "LM Studio provider registered");
    }

    builder.build()
}

/// Resolve the best available inference config for owner templates (spec 11.1).
///
/// Prefers Anthropic > OpenAI > LM Studio > local, based on which providers
/// are configured. Cloud providers set `owner_acknowledged_cloud_risk: true`
/// since the owner explicitly provided the API key.
fn resolve_owner_inference_config(llm: &LlmConfig) -> InferenceConfig {
    if let Some(ref anthropic) = llm.anthropic {
        info!(model = %anthropic.model, "owner templates will use Anthropic provider");
        InferenceConfig {
            provider: "anthropic".to_string(),
            model: anthropic.model.clone(),
            owner_acknowledged_cloud_risk: true,
        }
    } else if let Some(ref openai) = llm.openai {
        info!(model = %openai.model, "owner templates will use OpenAI provider");
        InferenceConfig {
            provider: "openai".to_string(),
            model: openai.model.clone(),
            owner_acknowledged_cloud_risk: true,
        }
    } else if let Some(ref lmstudio) = llm.lmstudio {
        info!(model = %lmstudio.model, "owner templates will use LM Studio provider");
        InferenceConfig {
            provider: "lmstudio".to_string(),
            model: lmstudio.model.clone(),
            owner_acknowledged_cloud_risk: false,
        }
    } else {
        info!(model = %llm.local.model, "owner templates will use local Ollama provider (no cloud keys configured)");
        InferenceConfig {
            provider: "local".to_string(),
            model: llm.local.model.clone(),
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
            "memory.save".to_string(),
            "admin.*".to_string(),
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
            "memory.save".to_string(),
            "admin.*".to_string(),
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

/// Create tool registry with Phase 2 + Phase 3 tools (spec 6.11, 8.2, 12.2).
///
/// Uses two-phase init: builds base tools first, then creates AdminTool
/// with MCP manager for dynamic integration support.
/// Returns `(Arc<ToolRegistry>, Arc<McpServerManager>)` — the manager is
/// needed for graceful shutdown and auto-start of configured MCP servers.
fn create_tool_registry(
    vault: Arc<dyn pfar::kernel::vault::SecretStore>,
    templates: Arc<TemplateRegistry>,
    journal: Arc<TaskJournal>,
    audit: Arc<AuditLogger>,
) -> (Arc<ToolRegistry>, Arc<McpServerManager>) {
    // Phase 1: Base tools (email, calendar, memory) — snapshot for AdminTool listing.
    let base_registry = ToolRegistry::new();
    base_registry.register(Arc::new(CalendarTool::new()));
    base_registry.register(Arc::new(EmailTool::new()));
    base_registry.register(Arc::new(MemoryTool::new(Arc::clone(&journal))));
    let base_tools = Arc::new(base_registry);

    // Phase 2: Final registry — MCP tools will be registered here dynamically.
    let registry = Arc::new(ToolRegistry::new());
    registry.register(Arc::new(CalendarTool::new()));
    registry.register(Arc::new(EmailTool::new()));
    registry.register(Arc::new(MemoryTool::new(journal)));

    // Phase 3: Create MCP server manager with ref to the final registry
    // (feature-dynamic-integrations).
    let mcp_manager = Arc::new(McpServerManager::new(
        Arc::clone(&registry),
        Arc::clone(&vault),
        audit,
    ));

    // Phase 4: AdminTool gets refs to base tools for listing + MCP manager.
    let admin =
        AdminTool::new(vault, base_tools, templates).with_mcp_manager(Arc::clone(&mcp_manager));
    registry.register(Arc::new(admin));

    (registry, mcp_manager)
}
