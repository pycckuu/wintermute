//! Model provider abstractions and shared request/response types.

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod anthropic;
pub mod ollama;
pub mod router;

/// Errors returned by model providers.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// HTTP transport failure.
    #[error("provider request failed: {0}")]
    Request(#[from] reqwest::Error),
    /// Response did not match expected schema.
    #[error("provider response parse error: {0}")]
    Parse(String),
    /// Upstream provider responded with an error status.
    #[error("provider returned non-success status {status}: {body}")]
    HttpStatus {
        /// HTTP status code.
        status: u16,
        /// Raw response body.
        body: String,
    },
    /// Provider cannot satisfy the request with current configuration.
    #[error("provider unavailable: {0}")]
    Unavailable(String),
}

/// Check HTTP response status and return body text or a structured error.
///
/// # Errors
///
/// Returns `ProviderError::Request` on transport failure, `ProviderError::HttpStatus` on non-2xx.
pub async fn check_http_response(response: reqwest::Response) -> Result<String, ProviderError> {
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(ProviderError::HttpStatus {
            status: status.as_u16(),
            body: sanitize_http_error_body(&body),
        });
    }
    Ok(body)
}

fn sanitize_http_error_body(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");

    let mut sanitized = collapsed;
    for pattern in [
        r"sk-ant-[A-Za-z0-9_\-]{10,}",
        r"sk-[A-Za-z0-9]{32,}",
        r"ghp_[A-Za-z0-9]{20,}",
        r"glpat-[A-Za-z0-9_\-]{16,}",
        r"xoxb-[A-Za-z0-9\-]{20,}",
    ] {
        if let Ok(regex) = Regex::new(pattern) {
            sanitized = regex.replace_all(&sanitized, "[REDACTED]").into_owned();
        }
    }

    const MAX_ERROR_BODY_CHARS: usize = 256;
    if sanitized.chars().count() > MAX_ERROR_BODY_CHARS {
        let shortened = sanitized
            .chars()
            .take(MAX_ERROR_BODY_CHARS)
            .collect::<String>();
        return format!("{shortened}...[truncated]");
    }

    sanitized
}

/// A chat message in completion context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompletionMessage {
    /// Role of this message.
    pub role: MessageRole,
    /// Text content.
    pub content: String,
}

/// Message role for chat-completion requests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    /// A system instruction.
    System,
    /// A user message.
    User,
    /// A model response.
    Assistant,
    /// A tool result message.
    Tool,
}

/// Tool schema advertised to providers that support native tool calling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    /// Tool name.
    pub name: String,
    /// User-facing description.
    pub description: String,
    /// JSON schema for parameters.
    pub parameters: Value,
}

/// A tool invocation returned by a provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    /// Provider-assigned call identifier.
    pub id: Option<String>,
    /// Tool name to invoke.
    pub name: String,
    /// Parsed JSON arguments for the tool call.
    pub arguments: Value,
}

/// Token usage counters returned by providers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TokenUsage {
    /// Input token count.
    pub input_tokens: u64,
    /// Output token count.
    pub output_tokens: u64,
    /// Total token count.
    pub total_tokens: u64,
}

/// Completion request passed to providers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompletionRequest {
    /// Context messages in order.
    pub messages: Vec<CompletionMessage>,
    /// Optional tool list for native tool-calling models.
    pub tools: Vec<ToolDefinition>,
    /// Optional model temperature.
    pub temperature: Option<f32>,
    /// Optional token cap.
    pub max_tokens: Option<u32>,
    /// Whether streaming was requested.
    pub stream: bool,
}

/// Completion response returned by providers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompletionResponse {
    /// Assistant textual response.
    pub content: String,
    /// Tool calls returned by the provider.
    pub tool_calls: Vec<ToolCall>,
    /// Optional token accounting.
    pub usage: Option<TokenUsage>,
}

/// Unified provider interface used by model routing.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Produce a completion for the given request.
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError>;
    /// Whether this provider supports native tool-calling.
    fn supports_tool_calling(&self) -> bool;
    /// Whether this provider supports streaming responses.
    fn supports_streaming(&self) -> bool;
    /// Returns `<provider>/<model>` identifier for this instance.
    fn model_id(&self) -> &str;
}
