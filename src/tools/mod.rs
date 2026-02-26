//! Tool routing: dispatches tool calls to core or dynamic implementations.
//!
//! The [`ToolRouter`] is the single entry point for all tool execution.
//! Every tool result passes through the [`Redactor`] before being returned,
//! ensuring no secrets leak into LLM context.

pub mod browser;
pub mod browser_bridge;
pub mod core;
pub mod create_tool;
pub mod docker;
pub mod registry;

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::agent::policy::{PolicyError, RateLimiter};
use crate::agent::TelegramOutbound;
use crate::executor::redactor::Redactor;
use crate::executor::Executor;
use crate::memory::MemoryEngine;
use crate::providers::ToolDefinition;
use crate::tools::browser::BrowserBridge;

// ---------------------------------------------------------------------------
// ToolResult
// ---------------------------------------------------------------------------

/// The result of executing a tool, returned to the LLM context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// The output content from the tool.
    pub content: String,
    /// Whether the tool execution resulted in an error.
    pub is_error: bool,
}

impl ToolResult {
    /// Create a successful tool result.
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    /// Create an error tool result.
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

// ---------------------------------------------------------------------------
// ToolError
// ---------------------------------------------------------------------------

/// Errors from tool execution.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// The requested tool was not found.
    #[error("unknown tool: {0}")]
    UnknownTool(String),

    /// Tool execution failed.
    #[error("execution failed: {0}")]
    ExecutionFailed(String),

    /// Rate limit exceeded for the tool.
    #[error("rate limited for tool {tool}: {detail}")]
    RateLimited {
        /// The tool that was rate-limited.
        tool: String,
        /// Details about the limit.
        detail: String,
    },

    /// Invalid input provided to the tool.
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

impl From<PolicyError> for ToolError {
    fn from(err: PolicyError) -> Self {
        match err {
            PolicyError::RateLimited { tool, detail } => Self::RateLimited { tool, detail },
            PolicyError::SsrfBlocked { url, reason } => {
                Self::ExecutionFailed(format!("SSRF blocked for {url}: {reason}"))
            }
            PolicyError::Forbidden(msg) => Self::ExecutionFailed(msg),
        }
    }
}

// ---------------------------------------------------------------------------
// ToolRouter
// ---------------------------------------------------------------------------

/// Central tool router that dispatches calls and enforces redaction.
///
/// All tool output (both core and dynamic) passes through the [`Redactor`]
/// before being returned, ensuring no secrets leak into LLM context.
pub struct ToolRouter {
    /// Executor for running commands.
    executor: Arc<dyn Executor>,
    /// Redactor for sanitizing output.
    redactor: Redactor,
    /// Memory engine for search/save.
    memory: Arc<MemoryEngine>,
    /// Dynamic tool registry.
    registry: Arc<registry::DynamicToolRegistry>,
    /// Telegram outbound channel.
    telegram_tx: Option<mpsc::Sender<TelegramOutbound>>,
    /// Rate limiter for web_fetch.
    fetch_limiter: Arc<RateLimiter>,
    /// Rate limiter for web_request.
    request_limiter: Arc<RateLimiter>,
    /// Rate limiter for browser actions.
    browser_limiter: Arc<RateLimiter>,
    /// Optional browser bridge; when None, browser tool returns unavailable.
    browser_bridge: Option<Arc<dyn BrowserBridge>>,
    /// Optional Docker client for docker_manage; when None, tool returns unavailable.
    docker_client: Option<bollard::Docker>,
    /// Maximum file download size in bytes for web_fetch save_to mode.
    max_download_bytes: Option<u64>,
}

impl std::fmt::Debug for ToolRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRouter")
            .field("has_telegram_tx", &self.telegram_tx.is_some())
            .finish_non_exhaustive()
    }
}

impl ToolRouter {
    /// Create a new tool router with all dependencies.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        executor: Arc<dyn Executor>,
        redactor: Redactor,
        memory: Arc<MemoryEngine>,
        registry: Arc<registry::DynamicToolRegistry>,
        telegram_tx: Option<mpsc::Sender<TelegramOutbound>>,
        fetch_limiter: Arc<RateLimiter>,
        request_limiter: Arc<RateLimiter>,
        browser_limiter: Arc<RateLimiter>,
        browser_bridge: Option<Arc<dyn BrowserBridge>>,
        docker_client: Option<bollard::Docker>,
        max_download_bytes: Option<u64>,
    ) -> Self {
        Self {
            executor,
            redactor,
            memory,
            registry,
            telegram_tx,
            fetch_limiter,
            request_limiter,
            browser_limiter,
            browser_bridge,
            docker_client,
            max_download_bytes,
        }
    }

    /// Execute a tool by name with the given JSON input.
    ///
    /// Dispatches to core tools first, then dynamic registry.
    /// All output is redacted before returning.
    pub async fn execute(&self, name: &str, input: &serde_json::Value) -> ToolResult {
        self.execute_for_user(name, input, None).await
    }

    /// Return the shared output redactor used by this router.
    pub fn redactor(&self) -> &Redactor {
        &self.redactor
    }

    /// Execute a tool with optional session user context.
    ///
    /// When `session_user_id` is provided, it is used by tools that need an
    /// authenticated user target (for example `send_telegram`).
    pub async fn execute_for_user(
        &self,
        name: &str,
        input: &serde_json::Value,
        session_user_id: Option<i64>,
    ) -> ToolResult {
        debug!(tool = name, "dispatching tool call");

        let raw_result = self.dispatch(name, input, session_user_id).await;

        // CRITICAL: ALL output passes through the redactor.
        let redacted_content = self.redactor.redact(&raw_result.content);
        ToolResult {
            content: redacted_content,
            is_error: raw_result.is_error,
        }
    }

    /// Dispatch to the appropriate tool implementation without redaction.
    async fn dispatch(
        &self,
        name: &str,
        input: &serde_json::Value,
        session_user_id: Option<i64>,
    ) -> ToolResult {
        match name {
            "execute_command" => {
                into_tool_result(core::execute_command(&*self.executor, input).await)
            }
            "web_fetch" => into_tool_result(
                core::web_fetch(input, &self.fetch_limiter, self.max_download_bytes).await,
            ),
            "web_request" => {
                into_tool_result(core::web_request(input, &self.request_limiter).await)
            }
            "browser" => into_tool_result(
                browser::run_browser(input, &self.browser_limiter, self.browser_bridge.as_deref())
                    .await,
            ),
            "memory_search" => into_tool_result(core::memory_search(&self.memory, input).await),
            "memory_save" => into_tool_result(core::memory_save(&self.memory, input).await),
            "send_telegram" => {
                let resolved_user_id =
                    session_user_id.or_else(|| input.get("user_id").and_then(|v| v.as_i64()));
                match (&self.telegram_tx, resolved_user_id) {
                    (Some(tx), Some(user_id)) => into_tool_result(
                        core::send_telegram(tx, user_id, input, self.executor.workspace_dir())
                            .await,
                    ),
                    (Some(_), None) => {
                        ToolResult::error("send_telegram requires a session user context")
                    }
                    (None, _) => ToolResult::error("telegram not configured"),
                }
            }
            "create_tool" => into_tool_result(
                core::handle_create_tool(&*self.executor, &self.registry, input).await,
            ),
            "docker_manage" => match &self.docker_client {
                Some(client) => into_tool_result(docker::docker_manage(client, input).await),
                None => ToolResult::error("docker not available"),
            },
            _ => {
                if let Some(schema) = self.registry.get(name) {
                    self.registry.record_usage(name);
                    self.execute_dynamic(name, &schema, input).await
                } else {
                    warn!(tool = name, "unknown tool requested");
                    ToolResult::error(format!("Unknown tool: {name}"))
                }
            }
        }
    }

    /// Execute a dynamically registered tool by running its script.
    async fn execute_dynamic(
        &self,
        name: &str,
        schema: &registry::DynamicToolSchema,
        input: &serde_json::Value,
    ) -> ToolResult {
        let scripts_dir = self.executor.scripts_dir().display();
        let input_json = input.to_string();
        let escaped_input = crate::executor::docker::shell_escape(&input_json);

        let command = format!("echo {escaped_input} | python3 {scripts_dir}/{name}.py");

        let opts = crate::executor::ExecOptions {
            timeout: std::time::Duration::from_secs(schema.timeout_secs),
            working_dir: None,
        };

        match self.executor.execute(&command, opts).await {
            Ok(result) => {
                if result.success() {
                    ToolResult::success(result.stdout)
                } else {
                    ToolResult::error(result.output())
                }
            }
            Err(e) => ToolResult::error(format!("dynamic tool execution failed: {e}")),
        }
    }
}

/// Convert a tool function result into a [`ToolResult`].
fn into_tool_result(result: Result<String, ToolError>) -> ToolResult {
    match result {
        Ok(output) => ToolResult::success(output),
        Err(e) => ToolResult::error(e.to_string()),
    }
}

impl ToolRouter {
    /// Return tool definitions for core tools plus up to `max_dynamic` dynamic tools.
    ///
    /// Dynamic tools are ranked by relevance (from `query` text) and recency (last-used).
    /// When `query` is provided, tools whose descriptions overlap with the query are
    /// preferred; otherwise tools are ordered by most recently used first.
    pub fn tool_definitions(&self, max_dynamic: u32, query: Option<&str>) -> Vec<ToolDefinition> {
        let mut defs = core::core_tool_definitions();
        if self.browser_bridge.is_none() {
            defs.retain(|def| def.name != "browser");
        }
        if self.docker_client.is_some() {
            defs.push(docker::docker_manage_tool_definition());
        }
        let max_dynamic = match usize::try_from(max_dynamic) {
            Ok(value) => value,
            Err(_) => usize::MAX,
        };
        let dynamic = self.registry.ranked_definitions(max_dynamic, query);
        defs.extend(dynamic);
        defs
    }

    /// Number of dynamic tools currently registered.
    pub fn dynamic_tool_count(&self) -> usize {
        self.registry.count()
    }
}
