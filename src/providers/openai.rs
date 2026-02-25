//! OpenAI provider implementation using the `/v1/chat/completions` API.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::credentials::OpenAiAuth;

use super::{
    check_http_response, CompletionRequest, CompletionResponse, ContentPart, LlmProvider,
    MessageContent, ProviderError, Role, StopReason, UsageStats,
};

const OPENAI_API_BASE: &str = "https://api.openai.com/v1/chat/completions";
const DEFAULT_MAX_TOKENS: u32 = 4096;

// ---------------------------------------------------------------------------
// Wire types (pub for integration testing)
// ---------------------------------------------------------------------------

/// OpenAI chat completions API request body.
#[doc(hidden)]
#[derive(Debug, Serialize)]
pub struct OpenAiRequest {
    /// Model identifier.
    pub model: String,
    /// Conversation messages.
    pub messages: Vec<OpenAiMessage>,
    /// Tool definitions.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Value>,
    /// Maximum completion tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Stop sequences.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
}

/// A message in OpenAI chat format.
#[doc(hidden)]
#[derive(Debug, Serialize)]
pub struct OpenAiMessage {
    /// Role (`system`, `user`, `assistant`, `tool`).
    pub role: String,
    /// Optional plain text content.
    pub content: Option<String>,
    /// Optional assistant tool calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAiToolCall>>,
    /// Optional tool call identifier when role is `tool`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// A tool call in OpenAI format.
#[doc(hidden)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiToolCall {
    /// Unique call identifier.
    pub id: String,
    /// Call type (always `function`).
    #[serde(rename = "type")]
    pub kind: String,
    /// Function call payload.
    pub function: OpenAiFunctionCall,
}

/// Function payload in OpenAI tool calls.
#[doc(hidden)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiFunctionCall {
    /// Function name.
    pub name: String,
    /// Function arguments encoded as a JSON string.
    pub arguments: String,
}

/// OpenAI chat completions API response body.
#[doc(hidden)]
#[derive(Debug, Deserialize)]
pub struct OpenAiResponse {
    /// Response choices.
    pub choices: Vec<OpenAiChoice>,
    /// Model that served the response.
    pub model: String,
    /// Token usage.
    pub usage: Option<OpenAiUsage>,
}

/// A response choice from OpenAI.
#[doc(hidden)]
#[derive(Debug, Deserialize)]
pub struct OpenAiChoice {
    /// Assistant message for this choice.
    pub message: OpenAiResponseMessage,
    /// Why generation stopped.
    pub finish_reason: Option<String>,
}

/// Assistant message from OpenAI.
#[doc(hidden)]
#[derive(Debug, Deserialize)]
pub struct OpenAiResponseMessage {
    /// Optional text content.
    pub content: Option<String>,
    /// Optional tool calls.
    pub tool_calls: Option<Vec<OpenAiToolCall>>,
}

/// OpenAI usage statistics.
#[doc(hidden)]
#[derive(Debug, Deserialize)]
pub struct OpenAiUsage {
    /// Prompt token count.
    pub prompt_tokens: Option<u32>,
    /// Completion token count.
    pub completion_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// OpenAI chat completions API provider.
#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    model_spec: String,
    model_name: String,
    auth: OpenAiAuth,
    client: reqwest::Client,
}

impl OpenAiProvider {
    /// Create a new OpenAI provider instance.
    pub fn new(model_spec: String, model_name: String, auth: OpenAiAuth) -> Self {
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

/// Build an OpenAI API request from a completion request.
#[doc(hidden)]
pub fn build_request(model: &str, request: &CompletionRequest) -> OpenAiRequest {
    let mut messages: Vec<OpenAiMessage> = Vec::new();

    if let Some(system) = &request.system {
        messages.push(OpenAiMessage {
            role: "system".to_owned(),
            content: Some(system.clone()),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    for msg in &request.messages {
        match &msg.content {
            MessageContent::Text(text) => {
                messages.push(OpenAiMessage {
                    role: role_to_openai(msg.role).to_owned(),
                    content: Some(text.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            MessageContent::Parts(parts) => {
                let mut text_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<OpenAiToolCall> = Vec::new();
                let mut tool_results: Vec<(String, String)> = Vec::new();

                for part in parts {
                    match part {
                        ContentPart::Text { text } => text_parts.push(text.clone()),
                        ContentPart::ToolUse { id, name, input } => {
                            let arguments = match serde_json::to_string(input) {
                                Ok(serialized) => serialized,
                                Err(_) => "{}".to_owned(),
                            };
                            tool_calls.push(OpenAiToolCall {
                                id: id.clone(),
                                kind: "function".to_owned(),
                                function: OpenAiFunctionCall {
                                    name: name.clone(),
                                    arguments,
                                },
                            });
                        }
                        ContentPart::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            tool_results.push((tool_use_id.clone(), content.clone()));
                        }
                    }
                }

                if !text_parts.is_empty() || !tool_calls.is_empty() {
                    let content = if text_parts.is_empty() {
                        None
                    } else {
                        Some(text_parts.join("\n"))
                    };
                    messages.push(OpenAiMessage {
                        role: role_to_openai(msg.role).to_owned(),
                        content,
                        tool_calls: if tool_calls.is_empty() {
                            None
                        } else {
                            Some(tool_calls)
                        },
                        tool_call_id: None,
                    });
                }

                for (tool_use_id, content) in tool_results {
                    messages.push(OpenAiMessage {
                        role: "tool".to_owned(),
                        content: Some(content),
                        tool_calls: None,
                        tool_call_id: Some(tool_use_id),
                    });
                }
            }
        }
    }

    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                }
            })
        })
        .collect();

    OpenAiRequest {
        model: model.to_owned(),
        messages,
        tools,
        max_tokens: Some(request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
        stop: request.stop_sequences.clone(),
    }
}

/// Parse an OpenAI API response into a completion response.
///
/// # Errors
///
/// Returns `ProviderError::Parse` if the response cannot be deserialized or
/// the first choice contains invalid tool call argument JSON.
#[doc(hidden)]
pub fn parse_response(body: &str) -> Result<CompletionResponse, ProviderError> {
    let resp: OpenAiResponse =
        serde_json::from_str(body).map_err(|e| ProviderError::Parse(e.to_string()))?;

    let choice = resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| ProviderError::Parse("missing choices[0]".to_owned()))?;

    let mut content: Vec<ContentPart> = Vec::new();
    if let Some(text) = choice.message.content {
        if !text.is_empty() {
            content.push(ContentPart::Text { text });
        }
    }

    if let Some(tool_calls) = choice.message.tool_calls {
        for call in tool_calls {
            let input = serde_json::from_str::<Value>(&call.function.arguments).map_err(|e| {
                ProviderError::Parse(format!(
                    "failed to parse tool call arguments for '{}': {e}",
                    call.function.name
                ))
            })?;
            content.push(ContentPart::ToolUse {
                id: call.id,
                name: call.function.name,
                input,
            });
        }
    }

    let stop_reason = match choice.finish_reason.as_deref() {
        Some("stop") => StopReason::EndTurn,
        Some("tool_calls") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        Some("content_filter") => StopReason::StopSequence,
        Some(other) => StopReason::Other(other.to_owned()),
        None => StopReason::EndTurn,
    };

    let usage = UsageStats {
        input_tokens: resp
            .usage
            .as_ref()
            .and_then(|u| u.prompt_tokens)
            .unwrap_or(0),
        output_tokens: resp
            .usage
            .as_ref()
            .and_then(|u| u.completion_tokens)
            .unwrap_or(0),
    };

    Ok(CompletionResponse {
        content,
        stop_reason,
        usage,
        model: resp.model,
    })
}

fn role_to_openai(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

// ---------------------------------------------------------------------------
// Trait impl
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl LlmProvider for OpenAiProvider {
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let api_request = build_request(&self.model_name, &request);
        let bearer_token = match &self.auth {
            OpenAiAuth::OAuthToken(token) | OpenAiAuth::ApiKey(token) => token,
        };

        let response = self
            .client
            .post(OPENAI_API_BASE)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {bearer_token}"))
            .json(&api_request)
            .send()
            .await?;

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
