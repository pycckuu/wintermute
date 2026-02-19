//! Anthropic provider implementation using native tool calling.

use serde_json::{json, Value};

use super::{
    check_http_response, CompletionRequest, CompletionResponse, LlmProvider, MessageRole,
    ProviderError, TokenUsage, ToolCall,
};

const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic messages API provider.
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    model_spec: String,
    model_name: String,
    api_key: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Create a new Anthropic provider instance.
    pub fn new(model_spec: String, model_name: String, api_key: String) -> Self {
        Self {
            model_spec,
            model_name,
            api_key,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(|message| {
                json!({
                    "role": anthropic_role(&message.role),
                    "content": message.content,
                })
            })
            .collect();

        let mut body = json!({
            "model": self.model_name,
            "messages": messages,
            "stream": request.stream,
            "max_tokens": request.max_tokens.unwrap_or(1024),
        });

        if let Some(temperature) = request.temperature {
            body["temperature"] = json!(temperature);
        }

        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.parameters,
                    })
                })
                .collect();
            body["tools"] = Value::Array(tools);
        }

        let response = self
            .client
            .post(ANTHROPIC_API_BASE)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let payload = check_http_response(response).await?;

        let parsed: Value =
            serde_json::from_str(&payload).map_err(|e| ProviderError::Parse(e.to_string()))?;

        let content = parse_content_text(&parsed)?;
        let tool_calls = parse_tool_calls(&parsed)?;
        let usage = parse_usage(&parsed);

        Ok(CompletionResponse {
            content,
            tool_calls,
            usage,
        })
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

fn anthropic_role(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::System => "user",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "user",
    }
}

fn parse_content_text(value: &Value) -> Result<String, ProviderError> {
    let Some(content_items) = value.get("content").and_then(Value::as_array) else {
        return Err(ProviderError::Parse("missing content array".to_owned()));
    };

    let mut text = String::new();
    for item in content_items {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        if item_type == "text" {
            let part = item.get("text").and_then(Value::as_str).unwrap_or_default();
            text.push_str(part);
        }
    }

    Ok(text)
}

fn parse_tool_calls(value: &Value) -> Result<Vec<ToolCall>, ProviderError> {
    let Some(content_items) = value.get("content").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };

    let mut calls = Vec::new();
    for item in content_items {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        if item_type == "tool_use" {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| ProviderError::Parse("tool_use missing name".to_owned()))?
                .to_owned();
            let arguments = item.get("input").cloned().unwrap_or(Value::Null);
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            calls.push(ToolCall {
                id,
                name,
                arguments,
            });
        }
    }

    Ok(calls)
}

fn parse_usage(value: &Value) -> Option<TokenUsage> {
    let usage = value.get("usage")?;
    let input_tokens = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(TokenUsage {
        input_tokens,
        output_tokens,
        total_tokens: input_tokens.saturating_add(output_tokens),
    })
}
