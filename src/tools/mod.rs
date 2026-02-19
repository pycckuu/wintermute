//! Tool routing: dispatches tool calls to core or dynamic implementations.
//!
//! The [`ToolRouter`] is the single entry point for all tool execution.
//! Every tool result passes through the [`Redactor`] before being returned,
//! ensuring no secrets leak into LLM context.

pub mod core;
pub mod create_tool;
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
    /// Policy context for domain checks (used by agent loop before dispatch).
    pub policy_context: crate::agent::policy::PolicyContext,
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
        policy_context: crate::agent::policy::PolicyContext,
    ) -> Self {
        Self {
            executor,
            redactor,
            memory,
            registry,
            telegram_tx,
            fetch_limiter,
            request_limiter,
            policy_context,
        }
    }

    /// Execute a tool by name with the given JSON input.
    ///
    /// Dispatches to core tools first, then dynamic registry.
    /// All output is redacted before returning.
    pub async fn execute(&self, name: &str, input: &serde_json::Value) -> ToolResult {
        debug!(tool = name, "dispatching tool call");

        let raw_result = self.dispatch(name, input).await;

        // CRITICAL: ALL output passes through the redactor.
        let redacted_content = self.redactor.redact(&raw_result.content);
        ToolResult {
            content: redacted_content,
            is_error: raw_result.is_error,
        }
    }

    /// Dispatch to the appropriate tool implementation without redaction.
    async fn dispatch(&self, name: &str, input: &serde_json::Value) -> ToolResult {
        match name {
            "execute_command" => {
                into_tool_result(core::execute_command(&*self.executor, input).await)
            }
            "web_fetch" => into_tool_result(core::web_fetch(input, &self.fetch_limiter).await),
            "web_request" => {
                into_tool_result(core::web_request(input, &self.request_limiter).await)
            }
            "memory_search" => into_tool_result(core::memory_search(&self.memory, input).await),
            "memory_save" => into_tool_result(core::memory_save(&self.memory, input).await),
            "send_telegram" => {
                let user_id = input.get("user_id").and_then(|v| v.as_i64()).unwrap_or(0);
                match &self.telegram_tx {
                    Some(tx) => into_tool_result(core::send_telegram(tx, user_id, input).await),
                    None => ToolResult::error("telegram not configured"),
                }
            }
            "create_tool" => into_tool_result(
                core::handle_create_tool(&*self.executor, &self.registry, input).await,
            ),
            _ => {
                if let Some(schema) = self.registry.get(name) {
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
    /// Return tool definitions for core tools plus up to `max_dynamic` from the registry.
    pub fn tool_definitions(&self, max_dynamic: u32) -> Vec<ToolDefinition> {
        let mut defs = core::core_tool_definitions();
        let dynamic = self.registry.all_definitions();
        let limit = usize::try_from(max_dynamic).unwrap_or(usize::MAX);
        defs.extend(dynamic.into_iter().take(limit));
        defs
    }
}
