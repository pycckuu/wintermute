//! Ollama provider implementation using the `/api/chat` API.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    check_http_response, CompletionRequest, CompletionResponse, ContentPart, LlmProvider,
    ProviderError, Role, StopReason, UsageStats,
};

/// Default Ollama API base URL.
pub const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434";

// ---------------------------------------------------------------------------
// Wire types (pub for integration testing)
// ---------------------------------------------------------------------------

/// Ollama chat API request body.
#[doc(hidden)]
#[derive(Debug, Serialize)]
pub struct OllamaRequest {
    /// Model name.
    pub model: String,
    /// Conversation messages.
    pub messages: Vec<OllamaMessage>,
    /// Disable streaming for non-streaming calls.
    pub stream: bool,
    /// Tool definitions.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Value>,
    /// Generation options.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<OllamaOptions>,
}

/// A message in Ollama format.
#[doc(hidden)]
#[derive(Debug, Serialize, Deserialize)]
pub struct OllamaMessage {
    /// Role: "system", "user", "assistant", or "tool".
    pub role: String,
    /// Message content.
    pub content: String,
}

/// Ollama generation options.
#[doc(hidden)]
#[derive(Debug, Serialize)]
pub struct OllamaOptions {
    /// Maximum tokens to generate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_predict: Option<u32>,
}

/// Ollama chat API response body.
#[doc(hidden)]
#[derive(Debug, Deserialize)]
pub struct OllamaResponse {
    /// Response message.
    pub message: OllamaResponseMessage,
    /// Model that served the response.
    pub model: String,
    /// Input token count.
    pub prompt_eval_count: Option<u32>,
    /// Output token count.
    pub eval_count: Option<u32>,
}

/// The message part of an Ollama response.
#[doc(hidden)]
#[derive(Debug, Deserialize)]
pub struct OllamaResponseMessage {
    /// Message content.
    pub content: String,
    /// Tool calls, if any.
    pub tool_calls: Option<Vec<OllamaToolCall>>,
}

/// A tool call in Ollama format.
#[doc(hidden)]
#[derive(Debug, Deserialize)]
pub struct OllamaToolCall {
    /// Function call details.
    pub function: OllamaFunction,
}

/// Function call details in Ollama format.
#[doc(hidden)]
#[derive(Debug, Deserialize)]
pub struct OllamaFunction {
    /// Function name.
    pub name: String,
    /// Function arguments as JSON.
    pub arguments: Value,
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Ollama chat API provider.
#[derive(Debug, Clone)]
pub struct OllamaProvider {
    model_spec: String,
    /// Model name passed to Ollama.
    #[doc(hidden)]
    pub model: String,
    /// Base URL for the Ollama API.
    #[doc(hidden)]
    pub base_url: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    /// Create an Ollama provider for a model spec.
    pub fn new(model_spec: String, model_name: String) -> Self {
        Self {
            model_spec,
            model: model_name,
            base_url: DEFAULT_OLLAMA_URL.to_owned(),
            client: reqwest::Client::new(),
        }
    }

    /// Check whether the Ollama server is reachable.
    pub async fn is_available(&self) -> bool {
        let url = format!("{}/api/tags", self.base_url);
        self.client.get(&url).send().await.is_ok()
    }
}

// ---------------------------------------------------------------------------
// Request / Response builders (pub for integration testing)
// ---------------------------------------------------------------------------

/// Build an Ollama API request from a completion request.
#[doc(hidden)]
pub fn build_request(model: &str, request: &CompletionRequest) -> OllamaRequest {
    let mut messages: Vec<OllamaMessage> = Vec::new();

    // Inject system prompt as a system message if present.
    if let Some(system) = &request.system {
        messages.push(OllamaMessage {
            role: "system".to_owned(),
            content: system.clone(),
        });
    }

    for msg in &request.messages {
        let role = match msg.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        messages.push(OllamaMessage {
            role: role.to_owned(),
            content: msg.content.text(),
        });
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

    let options = request.max_tokens.map(|n| OllamaOptions {
        num_predict: Some(n),
    });

    OllamaRequest {
        model: model.to_owned(),
        messages,
        stream: false,
        tools,
        options,
    }
}

/// Parse an Ollama API response into a completion response.
///
/// # Errors
///
/// Returns `ProviderError::Parse` if the response cannot be deserialized.
#[doc(hidden)]
pub fn parse_response(body: &str) -> Result<CompletionResponse, ProviderError> {
    let resp: OllamaResponse =
        serde_json::from_str(body).map_err(|e| ProviderError::Parse(e.to_string()))?;

    let mut content = Vec::new();

    if !resp.message.content.is_empty() {
        content.push(ContentPart::Text {
            text: resp.message.content,
        });
    }

    let has_tool_calls = resp
        .message
        .tool_calls
        .as_ref()
        .is_some_and(|c| !c.is_empty());

    if let Some(tool_calls) = resp.message.tool_calls {
        for call in tool_calls {
            content.push(ContentPart::ToolUse {
                id: uuid::Uuid::new_v4().to_string(),
                name: call.function.name,
                input: call.function.arguments,
            });
        }
    }

    let stop_reason = if has_tool_calls {
        StopReason::ToolUse
    } else {
        StopReason::EndTurn
    };

    let usage = UsageStats {
        input_tokens: resp.prompt_eval_count.unwrap_or(0),
        output_tokens: resp.eval_count.unwrap_or(0),
    };

    Ok(CompletionResponse {
        content,
        stop_reason,
        usage,
        model: resp.model,
    })
}

// ---------------------------------------------------------------------------
// Trait impl
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl LlmProvider for OllamaProvider {
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let api_request = build_request(&self.model, &request);

        let url = format!("{}/api/chat", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("content-type", "application/json")
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
