//! Ollama provider implementation using native tool calling.

use serde_json::{json, Value};

use super::{
    check_http_response, CompletionRequest, CompletionResponse, LlmProvider, MessageRole,
    ProviderError, TokenUsage, ToolCall,
};

const OLLAMA_DEFAULT_ENDPOINT: &str = "http://127.0.0.1:11434/api/chat";

/// Ollama chat API provider.
#[derive(Debug, Clone)]
pub struct OllamaProvider {
    model_spec: String,
    model_name: String,
    endpoint: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    /// Create an Ollama provider for a model spec.
    pub fn new(model_spec: String, model_name: String) -> Self {
        Self {
            model_spec,
            model_name,
            endpoint: OLLAMA_DEFAULT_ENDPOINT.to_owned(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for OllamaProvider {
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(|message| {
                json!({
                    "role": ollama_role(&message.role),
                    "content": message.content,
                })
            })
            .collect();

        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    }
                })
            })
            .collect();

        let mut options = json!({});
        if let Some(temperature) = request.temperature {
            options["temperature"] = json!(temperature);
        }
        if let Some(max_tokens) = request.max_tokens {
            options["num_predict"] = json!(max_tokens);
        }

        let body = json!({
            "model": self.model_name,
            "messages": messages,
            "tools": tools,
            "stream": request.stream,
            "options": options,
        });

        let response = self
            .client
            .post(&self.endpoint)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let payload = check_http_response(response).await?;

        let parsed: Value =
            serde_json::from_str(&payload).map_err(|e| ProviderError::Parse(e.to_string()))?;

        let content = parsed
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();

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

fn ollama_role(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn parse_tool_calls(value: &Value) -> Result<Vec<ToolCall>, ProviderError> {
    let tool_calls = value
        .get("message")
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut calls = Vec::new();
    for tool_call in tool_calls {
        let function = tool_call.get("function").cloned().unwrap_or(Value::Null);
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| ProviderError::Parse("tool call missing function.name".to_owned()))?
            .to_owned();

        let arguments = function.get("arguments").cloned().unwrap_or(Value::Null);
        calls.push(ToolCall {
            id: None,
            name,
            arguments,
        });
    }

    Ok(calls)
}

fn parse_usage(value: &Value) -> Option<TokenUsage> {
    let prompt = value.get("prompt_eval_count").and_then(Value::as_u64);
    let output = value.get("eval_count").and_then(Value::as_u64);
    match (prompt, output) {
        (Some(input_tokens), Some(output_tokens)) => Some(TokenUsage {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens.saturating_add(output_tokens),
        }),
        _ => None,
    }
}
