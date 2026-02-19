//! Core tool implementations.
//!
//! Each tool is an async function that takes typed dependencies and JSON input,
//! returning either a formatted string on success or a [`ToolError`] on failure.
//! Tool definitions (name, description, JSON Schema) are returned by
//! [`core_tool_definitions`].

use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;
use tracing::{debug, warn};
use url::Url;

use crate::agent::policy::{ssrf_check, RateLimiter};
use crate::agent::TelegramOutbound;
use crate::executor::{ExecOptions, Executor};
use crate::memory::{Memory, MemoryEngine, MemoryKind, MemorySource, MemoryStatus};
use crate::providers::ToolDefinition;

use super::registry::DynamicToolRegistry;
use super::ToolError;

/// Maximum response body size in bytes for web_fetch and web_request.
const MAX_RESPONSE_BODY_BYTES: usize = 100 * 1024;

/// Maximum number of redirect hops for web_fetch.
const MAX_REDIRECT_HOPS: usize = 10;

/// Default command timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

// ---------------------------------------------------------------------------
// execute_command
// ---------------------------------------------------------------------------

/// Execute a shell command in the sandbox.
///
/// Extracts `command` (required) and optional `timeout_secs` (default 120) from input.
///
/// # Errors
///
/// Returns [`ToolError::InvalidInput`] if `command` is missing, or
/// [`ToolError::ExecutionFailed`] if the executor fails.
pub async fn execute_command(
    executor: &dyn Executor,
    input: &serde_json::Value,
) -> Result<String, ToolError> {
    let command = input
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: command".to_owned()))?;

    let timeout_secs = input
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);

    let opts = ExecOptions {
        timeout: Duration::from_secs(timeout_secs),
        working_dir: None,
    };

    debug!(command, timeout_secs, "executing command");

    let result = executor
        .execute(command, opts)
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

    Ok(format!(
        "Exit code: {}\nTimed out: {}\nStdout:\n{}\nStderr:\n{}",
        result
            .exit_code
            .map_or("none".to_owned(), |c| c.to_string()),
        result.timed_out,
        result.stdout,
        result.stderr,
    ))
}

// ---------------------------------------------------------------------------
// web_fetch
// ---------------------------------------------------------------------------

/// Fetch a URL via GET with SSRF protection and manual redirect following.
///
/// # Errors
///
/// Returns [`ToolError`] on rate limit, SSRF block, or request failure.
pub async fn web_fetch(
    input: &serde_json::Value,
    limiter: &RateLimiter,
) -> Result<String, ToolError> {
    let url_str = input
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: url".to_owned()))?;

    limiter.check("web_fetch")?;
    limiter.record();

    let mut current_url =
        Url::parse(url_str).map_err(|e| ToolError::InvalidInput(format!("invalid URL: {e}")))?;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to build HTTP client: {e}")))?;

    // Follow redirects manually, running SSRF check on each hop.
    for hop in 0..MAX_REDIRECT_HOPS {
        ssrf_check(&current_url).await?;

        debug!(url = %current_url, hop, "web_fetch request");

        let response = client
            .get(current_url.clone())
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("GET request failed: {e}")))?;

        let status = response.status();

        if status.is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    ToolError::ExecutionFailed(
                        "redirect response missing Location header".to_owned(),
                    )
                })?;

            current_url = current_url
                .join(location)
                .map_err(|e| ToolError::ExecutionFailed(format!("invalid redirect URL: {e}")))?;
            continue;
        }

        let body = response.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("failed to read response body: {e}"))
        })?;

        let truncated = truncate_body(&body, MAX_RESPONSE_BODY_BYTES);
        return Ok(truncated);
    }

    Err(ToolError::ExecutionFailed(format!(
        "too many redirects (>{MAX_REDIRECT_HOPS})"
    )))
}

// ---------------------------------------------------------------------------
// web_request
// ---------------------------------------------------------------------------

/// Send an HTTP request (POST/PUT/PATCH/DELETE) with SSRF protection.
///
/// # Errors
///
/// Returns [`ToolError`] on rate limit, SSRF block, or request failure.
pub async fn web_request(
    input: &serde_json::Value,
    limiter: &RateLimiter,
) -> Result<String, ToolError> {
    let url_str = input
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: url".to_owned()))?;

    let method = input
        .get("method")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: method".to_owned()))?;

    let body = input.get("body").and_then(|v| v.as_str());
    let headers = input.get("headers").and_then(|v| v.as_object());

    limiter.check("web_request")?;
    limiter.record();

    let url =
        Url::parse(url_str).map_err(|e| ToolError::InvalidInput(format!("invalid URL: {e}")))?;

    ssrf_check(&url).await?;

    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to build HTTP client: {e}")))?;

    let mut builder = match method.to_uppercase().as_str() {
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "PATCH" => client.patch(url),
        "DELETE" => client.delete(url),
        other => {
            return Err(ToolError::InvalidInput(format!(
                "unsupported HTTP method: {other}"
            )))
        }
    };

    if let Some(hdrs) = headers {
        for (key, value) in hdrs {
            if let Some(val_str) = value.as_str() {
                builder = builder.header(key, val_str);
            }
        }
    }

    if let Some(body_str) = body {
        builder = builder.body(body_str.to_owned());
    }

    debug!(method, url = url_str, "web_request");

    let response = builder
        .send()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("request failed: {e}")))?;

    let status = response.status();
    let response_body = response
        .text()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to read response body: {e}")))?;

    let truncated = truncate_body(&response_body, MAX_RESPONSE_BODY_BYTES);
    Ok(format!("Status: {status}\n\n{truncated}"))
}

// ---------------------------------------------------------------------------
// memory_search
// ---------------------------------------------------------------------------

/// Search memories using FTS5 and optional vector similarity.
///
/// # Errors
///
/// Returns [`ToolError::InvalidInput`] if `query` is missing, or
/// [`ToolError::ExecutionFailed`] on search failure.
pub async fn memory_search(
    memory: &MemoryEngine,
    input: &serde_json::Value,
) -> Result<String, ToolError> {
    let query = input
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: query".to_owned()))?;

    let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(10);

    let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);

    debug!(query, limit, "memory_search");

    let results = memory
        .search(query, limit_usize)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("memory search failed: {e}")))?;

    let formatted: Vec<serde_json::Value> = results
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "kind": m.kind,
                "content": m.content,
                "source": m.source,
                "created_at": m.created_at,
            })
        })
        .collect();

    serde_json::to_string_pretty(&formatted)
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to format results: {e}")))
}

// ---------------------------------------------------------------------------
// memory_save
// ---------------------------------------------------------------------------

/// Save a new memory entry.
///
/// # Errors
///
/// Returns [`ToolError::InvalidInput`] if required fields are missing, or
/// [`ToolError::ExecutionFailed`] on save failure.
pub async fn memory_save(
    memory: &MemoryEngine,
    input: &serde_json::Value,
) -> Result<String, ToolError> {
    let content = input
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: content".to_owned()))?;

    let kind_str = input
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: kind".to_owned()))?;

    let kind = MemoryKind::parse(kind_str)
        .map_err(|e| ToolError::InvalidInput(format!("invalid kind: {e}")))?;

    debug!(kind = kind_str, "memory_save");

    let entry = Memory {
        id: None,
        kind,
        content: content.to_owned(),
        metadata: None,
        status: MemoryStatus::Active,
        source: MemorySource::Agent,
        created_at: None,
        updated_at: None,
    };

    memory
        .save_memory(entry)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("memory save failed: {e}")))?;

    Ok(format!("Memory saved as {kind_str}"))
}

// ---------------------------------------------------------------------------
// send_telegram
// ---------------------------------------------------------------------------

/// Send a message to the user via Telegram.
///
/// # Errors
///
/// Returns [`ToolError::InvalidInput`] if `text` is missing, or
/// [`ToolError::ExecutionFailed`] if sending fails.
pub async fn send_telegram(
    tx: &mpsc::Sender<TelegramOutbound>,
    user_id: i64,
    input: &serde_json::Value,
) -> Result<String, ToolError> {
    let text = input
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: text".to_owned()))?;

    let file = input.get("file").and_then(|v| v.as_str());

    let outbound = TelegramOutbound {
        user_id,
        text: Some(text.to_owned()),
        file_path: file.map(ToOwned::to_owned),
        approval_keyboard: None,
    };

    // Use try_send to avoid blocking the async task.
    tx.try_send(outbound).map_err(|e| {
        warn!(error = %e, "telegram send failed");
        ToolError::ExecutionFailed(format!("failed to send telegram message: {e}"))
    })?;

    Ok("Message sent to Telegram".to_owned())
}

// ---------------------------------------------------------------------------
// create_tool (dispatches to create_tool module)
// ---------------------------------------------------------------------------

/// Handle the create_tool tool call by delegating to the create_tool module.
///
/// # Errors
///
/// Returns [`ToolError`] on validation or execution failure.
pub async fn handle_create_tool(
    executor: &dyn Executor,
    registry: &DynamicToolRegistry,
    input: &serde_json::Value,
) -> Result<String, ToolError> {
    let name = input
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: name".to_owned()))?;

    let description = input
        .get("description")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: description".to_owned()))?;

    let parameters_schema = input.get("parameters_schema").ok_or_else(|| {
        ToolError::InvalidInput("missing required field: parameters_schema".to_owned())
    })?;

    let implementation = input
        .get("implementation")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ToolError::InvalidInput("missing required field: implementation".to_owned())
        })?;

    let timeout_secs = input
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);

    super::create_tool::create_tool(
        executor,
        registry,
        name,
        description,
        parameters_schema,
        implementation,
        timeout_secs,
    )
    .await
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

/// Return definitions for all 7 core tools.
pub fn core_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "execute_command".to_owned(),
            description: "Execute a shell command in the sandbox container.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Maximum execution time in seconds (default 120).",
                        "default": 120
                    }
                },
                "required": ["command"]
            }),
        },
        ToolDefinition {
            name: "web_fetch".to_owned(),
            description: "Fetch a URL via GET request with SSRF protection.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch."
                    }
                },
                "required": ["url"]
            }),
        },
        ToolDefinition {
            name: "web_request".to_owned(),
            description:
                "Send an HTTP request (POST/PUT/PATCH/DELETE) with domain policy enforcement."
                    .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The target URL."
                    },
                    "method": {
                        "type": "string",
                        "enum": ["POST", "PUT", "PATCH", "DELETE"],
                        "description": "HTTP method."
                    },
                    "body": {
                        "type": "string",
                        "description": "Optional request body."
                    },
                    "headers": {
                        "type": "object",
                        "description": "Optional request headers as key-value pairs.",
                        "additionalProperties": { "type": "string" }
                    }
                },
                "required": ["url", "method"]
            }),
        },
        ToolDefinition {
            name: "memory_search".to_owned(),
            description: "Search memories using full-text and optional vector similarity."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results (default 10).",
                        "default": 10
                    }
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "memory_save".to_owned(),
            description: "Save a new memory entry for future retrieval.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "The memory content to save."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["fact", "procedure", "episode", "skill"],
                        "description": "The type of memory."
                    }
                },
                "required": ["content", "kind"]
            }),
        },
        ToolDefinition {
            name: "send_telegram".to_owned(),
            description: "Send a message or file to the user via Telegram.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The message text to send (HTML formatted)."
                    },
                    "file": {
                        "type": "string",
                        "description": "Optional file path to send as attachment."
                    }
                },
                "required": ["text"]
            }),
        },
        ToolDefinition {
            name: "create_tool".to_owned(),
            description: "Create or update a dynamic tool with a Python implementation.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Tool name (lowercase, underscores, max 64 chars)."
                    },
                    "description": {
                        "type": "string",
                        "description": "Human-readable description of the tool."
                    },
                    "parameters_schema": {
                        "type": "object",
                        "description": "JSON Schema for the tool's input parameters."
                    },
                    "implementation": {
                        "type": "string",
                        "description": "Python source code for the tool."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Maximum execution time in seconds (default 120).",
                        "default": 120
                    }
                },
                "required": ["name", "description", "parameters_schema", "implementation"]
            }),
        },
    ]
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Truncate a string to the given byte limit, appending a truncation notice.
fn truncate_body(body: &str, max_bytes: usize) -> String {
    if body.len() <= max_bytes {
        return body.to_owned();
    }

    // Find a valid char boundary at or before max_bytes.
    let mut end = max_bytes;
    while end > 0 && !body.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }

    let mut truncated = body[..end].to_owned();
    truncated.push_str("\n...[truncated]");
    truncated
}
