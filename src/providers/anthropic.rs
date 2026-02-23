//! Anthropic provider implementation using the `/v1/messages` API.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::credentials::AnthropicAuth;

use super::{
    check_http_response, CompletionRequest, CompletionResponse, ContentPart, LlmProvider,
    ProviderError, Role, StopReason, UsageStats,
};

const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Beta feature flag required for Anthropic OAuth authentication.
/// Can be removed once Anthropic graduates OAuth out of beta.
const ANTHROPIC_OAUTH_BETA: &str = "oauth-2025-04-20";

// ---------------------------------------------------------------------------
// Wire types (pub for integration testing)
// ---------------------------------------------------------------------------

/// Anthropic messages API request body.
#[doc(hidden)]
#[derive(Debug, Serialize)]
pub struct AnthropicRequest {
    /// Model identifier.
    pub model: String,
    /// Conversation messages.
    pub messages: Vec<AnthropicMessage>,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
    /// Optional system prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Tool definitions.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<AnthropicTool>,
    /// Stop sequences.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
}

/// A message in Anthropic format.
#[doc(hidden)]
#[derive(Debug, Serialize, Deserialize)]
pub struct AnthropicMessage {
    /// Role: "user" or "assistant".
    pub role: String,
    /// Content blocks.
    pub content: Value,
}

/// A tool definition in Anthropic format.
#[doc(hidden)]
#[derive(Debug, Serialize)]
pub struct AnthropicTool {
    /// Tool name.
    pub name: String,
    /// Tool description.
    pub description: String,
    /// JSON Schema for tool input.
    pub input_schema: Value,
}

/// Anthropic API response body.
#[doc(hidden)]
#[derive(Debug, Deserialize)]
pub struct AnthropicResponse {
    /// Content blocks in the response.
    pub content: Vec<AnthropicContentBlock>,
    /// Model that served the response.
    pub model: String,
    /// Why the model stopped generating.
    pub stop_reason: Option<String>,
    /// Token usage.
    pub usage: AnthropicUsage,
}

/// A content block in the Anthropic response.
#[doc(hidden)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContentBlock {
    /// Text content.
    Text {
        /// The text.
        text: String,
    },
    /// Tool use request.
    ToolUse {
        /// Call identifier.
        id: String,
        /// Tool name.
        name: String,
        /// Tool input.
        input: Value,
    },
}

/// Anthropic usage statistics.
#[doc(hidden)]
#[derive(Debug, Deserialize)]
pub struct AnthropicUsage {
    /// Input tokens consumed.
    pub input_tokens: u32,
    /// Output tokens generated.
    pub output_tokens: u32,
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Anthropic messages API provider.
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    model_spec: String,
    model_name: String,
    auth: AnthropicAuth,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Create a new Anthropic provider instance.
    pub fn new(model_spec: String, model_name: String, auth: AnthropicAuth) -> Self {
        Self {
            model_spec,
            model_name,
            auth,
            client: reqwest::Client::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Request / Response builders (pub for integration testing)
// ---------------------------------------------------------------------------

/// Build an Anthropic API request from a completion request.
#[doc(hidden)]
pub fn build_request(model: &str, request: &CompletionRequest) -> AnthropicRequest {
    let messages: Vec<AnthropicMessage> = request
        .messages
        .iter()
        .map(|msg| {
            let role = match msg.role {
                Role::System | Role::User | Role::Tool => "user",
                Role::Assistant => "assistant",
            };
            AnthropicMessage {
                role: role.to_owned(),
                content: match &msg.content {
                    super::MessageContent::Text(t) => Value::String(t.clone()),
                    super::MessageContent::Parts(parts) => {
                        Value::Array(parts.iter().map(content_part_to_value).collect())
                    }
                },
            }
        })
        .collect();

    let tools: Vec<AnthropicTool> = request
        .tools
        .iter()
        .map(|t| AnthropicTool {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: t.input_schema.clone(),
        })
        .collect();

    AnthropicRequest {
        model: model.to_owned(),
        messages,
        max_tokens: request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        system: request.system.clone(),
        tools,
        stop_sequences: request.stop_sequences.clone(),
    }
}

/// Parse an Anthropic API response into a completion response.
///
/// # Errors
///
/// Returns `ProviderError::Parse` if the response cannot be deserialized.
#[doc(hidden)]
pub fn parse_response(body: &str) -> Result<CompletionResponse, ProviderError> {
    let resp: AnthropicResponse =
        serde_json::from_str(body).map_err(|e| ProviderError::Parse(e.to_string()))?;

    let content: Vec<ContentPart> = resp
        .content
        .into_iter()
        .map(|block| match block {
            AnthropicContentBlock::Text { text } => ContentPart::Text { text },
            AnthropicContentBlock::ToolUse { id, name, input } => {
                ContentPart::ToolUse { id, name, input }
            }
        })
        .collect();

    let stop_reason = match resp.stop_reason.as_deref() {
        Some("end_turn") => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("stop_sequence") => StopReason::StopSequence,
        Some(other) => StopReason::Other(other.to_owned()),
        None => StopReason::EndTurn,
    };

    Ok(CompletionResponse {
        content,
        stop_reason,
        usage: UsageStats {
            input_tokens: resp.usage.input_tokens,
            output_tokens: resp.usage.output_tokens,
        },
        model: resp.model,
    })
}

fn content_part_to_value(part: &ContentPart) -> Value {
    match part {
        ContentPart::Text { text } => {
            serde_json::json!({"type": "text", "text": text})
        }
        ContentPart::ToolUse { id, name, input } => {
            serde_json::json!({"type": "tool_use", "id": id, "name": name, "input": input})
        }
        ContentPart::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": is_error,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Trait impl
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let api_request = build_request(&self.model_name, &request);

        let mut builder = self
            .client
            .post(ANTHROPIC_API_BASE)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json");

        match &self.auth {
            AnthropicAuth::OAuth { access_token, .. } => {
                builder = builder
                    .header("authorization", format!("Bearer {access_token}"))
                    .header("anthropic-beta", ANTHROPIC_OAUTH_BETA);
            }
            AnthropicAuth::ApiKey(key) => {
                builder = builder.header("x-api-key", key);
            }
        }

        let response = builder.json(&api_request).send().await?;

        let payload = check_http_response(response).await?;
        parse_response(&payload)
    }

    fn supports_tool_calling(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn model_id(&self) -> &str {
        &self.model_spec
    }
}
