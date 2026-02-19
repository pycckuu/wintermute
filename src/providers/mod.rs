//! LLM provider abstraction layer.
//!
//! Defines the [`LlmProvider`] trait and the shared request/response types
//! used by all provider implementations.
//!
//! Two providers are implemented:
//! - [`anthropic::AnthropicProvider`] — Anthropic `/v1/messages` API
//! - [`ollama::OllamaProvider`] — Ollama `/api/chat` API
//!
//! The [`router::ModelRouter`] resolves the correct provider for each call
//! based on context (skill override → role override → default).

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};

pub mod anthropic;
pub mod ollama;
pub mod router;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Conversation participant role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// System message.
    System,
    /// Human user message.
    User,
    /// Assistant (LLM) message.
    Assistant,
    /// Tool result (used after a tool call).
    Tool,
}

/// A message in a conversation with an LLM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// The role of the message author.
    pub role: Role,
    /// Message content — may be text or structured (tool calls/results).
    pub content: MessageContent,
}

/// The content of a message — text or structured parts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    /// Plain text content.
    Text(String),
    /// Structured content blocks (text, tool calls, tool results).
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    /// Extract plain text from the content, joining all text parts.
    pub fn text(&self) -> String {
        match self {
            Self::Text(t) => t.clone(),
            Self::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect(),
        }
    }
}

/// A single structured content part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Plain text.
    Text {
        /// The text content.
        text: String,
    },
    /// Tool use request from the assistant.
    ToolUse {
        /// Unique call identifier.
        id: String,
        /// Tool name.
        name: String,
        /// Tool input as JSON.
        input: serde_json::Value,
    },
    /// Result of a tool call.
    ToolResult {
        /// Matching call identifier.
        tool_use_id: String,
        /// Result content.
        content: String,
        /// Whether the tool reported an error.
        is_error: bool,
    },
}

/// JSON Schema definition for a tool the LLM can call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool name (must match tool router registration).
    pub name: String,
    /// Description shown to the LLM.
    pub description: String,
    /// JSON Schema object for the tool's parameters.
    pub input_schema: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Request / Response
// ---------------------------------------------------------------------------

/// A request to an LLM provider for a completion.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// Conversation history including the latest user message.
    pub messages: Vec<Message>,
    /// System prompt (injected before messages).
    pub system: Option<String>,
    /// Tools available to the LLM for this call.
    pub tools: Vec<ToolDefinition>,
    /// Maximum tokens in the response.
    pub max_tokens: Option<u32>,
    /// Stop sequences.
    pub stop_sequences: Vec<String>,
}

/// The reason a completion stopped generating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// Normal end of turn.
    EndTurn,
    /// The model wants to call a tool.
    ToolUse,
    /// Max token limit reached.
    MaxTokens,
    /// A stop sequence was hit.
    StopSequence,
    /// Provider-specific other reason.
    Other(String),
}

/// Usage statistics for a completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UsageStats {
    /// Tokens used in the prompt/input.
    pub input_tokens: u32,
    /// Tokens generated in the response.
    pub output_tokens: u32,
}

/// The response from an LLM provider.
#[derive(Debug, Clone)]
pub struct CompletionResponse {
    /// Response content (text and/or tool calls).
    pub content: Vec<ContentPart>,
    /// Why the model stopped.
    pub stop_reason: StopReason,
    /// Token usage for budget tracking.
    pub usage: UsageStats,
    /// The model identifier that served this response.
    pub model: String,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// HTTP helpers (useful for all providers)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Core LLM provider interface.
///
/// All provider implementations must be `Send + Sync` to allow use
/// across async task boundaries in the agent loop.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Request a completion from the LLM.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] on API, network, or parse failure.
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError>;

    /// Whether this provider supports native tool calling.
    fn supports_tool_calling(&self) -> bool;

    /// Whether this provider supports streaming responses.
    fn supports_streaming(&self) -> bool;

    /// The model identifier string this provider is instantiated for.
    fn model_id(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

/// Parse a provider string like `"anthropic/claude-sonnet"` into components.
///
/// Returns `(provider_name, model_name)`.
///
/// # Errors
///
/// Returns an error if the string does not contain exactly one `/` separator.
pub fn parse_provider_string(s: &str) -> anyhow::Result<(&str, &str)> {
    let (provider, model) = s.split_once('/').ok_or_else(|| {
        anyhow::anyhow!("invalid provider string: {s:?}, expected format 'provider/model'")
    })?;
    if provider.is_empty() || model.is_empty() {
        anyhow::bail!("invalid provider string: {s:?}, both provider and model must be non-empty");
    }
    Ok((provider, model))
}
