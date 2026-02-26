//! OpenAI provider wire format tests.

use serde_json::json;
use wintermute::credentials::OpenAiAuth;
use wintermute::providers::openai::{build_request, parse_response, OpenAiProvider};
use wintermute::providers::{
    CompletionRequest, ContentPart, LlmProvider, Message, MessageContent, Role, StopReason,
};

fn simple_request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![Message {
            role: Role::User,
            content: MessageContent::Text("Hello".to_owned()),
        }],
        system: Some("You are helpful.".to_owned()),
        tools: vec![],
        max_tokens: Some(256),
        stop_sequences: vec![],
    }
}

#[test]
fn build_request_sets_model_system_and_max_tokens() {
    let req = build_request("gpt-5", &simple_request());
    assert_eq!(req.model, "gpt-5");
    assert_eq!(req.max_tokens, Some(256));
    assert_eq!(req.messages.len(), 2);
    assert_eq!(req.messages[0].role, "system");
    assert_eq!(req.messages[0].content, Some("You are helpful.".to_owned()));
    assert_eq!(req.messages[1].role, "user");
    assert_eq!(req.messages[1].content, Some("Hello".to_owned()));
}

#[test]
fn build_request_maps_tool_use_and_tool_result_blocks() {
    let request = CompletionRequest {
        messages: vec![
            Message {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: "checking weather".to_owned(),
                    },
                    ContentPart::ToolUse {
                        id: "call_1".to_owned(),
                        name: "weather_lookup".to_owned(),
                        input: json!({"city": "Paris"}),
                    },
                ]),
            },
            Message {
                role: Role::User,
                content: MessageContent::Parts(vec![ContentPart::ToolResult {
                    tool_use_id: "call_1".to_owned(),
                    content: "sunny".to_owned(),
                    is_error: false,
                }]),
            },
        ],
        system: None,
        tools: vec![],
        max_tokens: None,
        stop_sequences: vec![],
    };

    let req = build_request("gpt-5", &request);
    assert_eq!(req.max_tokens, Some(4096));
    assert_eq!(req.messages.len(), 2);

    let assistant = &req.messages[0];
    assert_eq!(assistant.role, "assistant");
    assert_eq!(assistant.content, Some("checking weather".to_owned()));
    let calls = assistant
        .tool_calls
        .as_ref()
        .expect("assistant message should include tool calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_1");
    assert_eq!(calls[0].function.name, "weather_lookup");
    let args: serde_json::Value =
        serde_json::from_str(&calls[0].function.arguments).expect("arguments should be JSON");
    assert_eq!(args["city"], "Paris");

    let tool_message = &req.messages[1];
    assert_eq!(tool_message.role, "tool");
    assert_eq!(tool_message.tool_call_id, Some("call_1".to_owned()));
    assert_eq!(tool_message.content, Some("sunny".to_owned()));
}

#[test]
fn parse_response_text_only() {
    let body = json!({
        "choices": [{
            "message": {"role": "assistant", "content": "Hello world"},
            "finish_reason": "stop"
        }],
        "model": "gpt-5",
        "usage": {"prompt_tokens": 10, "completion_tokens": 5}
    });

    let resp = parse_response(&body.to_string()).expect("should parse");
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(resp.model, "gpt-5");
    assert_eq!(resp.usage.input_tokens, 10);
    assert_eq!(resp.usage.output_tokens, 5);
    assert!(matches!(&resp.content[0], ContentPart::Text { text } if text == "Hello world"));
}

#[test]
fn parse_response_with_tool_calls() {
    let body = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "Let me check.",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "weather_lookup",
                        "arguments": "{\"city\":\"Paris\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "model": "gpt-5",
        "usage": {"prompt_tokens": 20, "completion_tokens": 7}
    });

    let resp = parse_response(&body.to_string()).expect("should parse");
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    assert_eq!(resp.content.len(), 2);
    assert!(matches!(&resp.content[0], ContentPart::Text { text } if text == "Let me check."));
    match &resp.content[1] {
        ContentPart::ToolUse { id, name, input } => {
            assert_eq!(id, "call_1");
            assert_eq!(name, "weather_lookup");
            assert_eq!(input["city"], "Paris");
        }
        other => panic!("expected ToolUse, got {other:?}"),
    }
}

#[test]
fn parse_response_invalid_tool_arguments() {
    let body = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "bad_args",
                        "arguments": "not json"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "model": "gpt-5"
    });

    let result = parse_response(&body.to_string());
    assert!(result.is_err());
}

#[test]
fn parse_response_without_choices_is_error() {
    let body = json!({
        "choices": [],
        "model": "gpt-5"
    });
    let result = parse_response(&body.to_string());
    assert!(result.is_err());
}

#[test]
fn parse_response_defaults_usage_to_zero() {
    let body = json!({
        "choices": [{
            "message": {"role": "assistant", "content": "ok"},
            "finish_reason": "stop"
        }],
        "model": "gpt-5"
    });
    let resp = parse_response(&body.to_string()).expect("should parse");
    assert_eq!(resp.usage.input_tokens, 0);
    assert_eq!(resp.usage.output_tokens, 0);
}

#[test]
fn parse_response_content_filter_maps_to_other() {
    let body = json!({
        "choices": [{
            "message": {"role": "assistant", "content": "I can't help with that."},
            "finish_reason": "content_filter"
        }],
        "model": "gpt-5",
        "usage": {"prompt_tokens": 5, "completion_tokens": 8}
    });

    let resp = parse_response(&body.to_string()).expect("should parse");
    assert_eq!(
        resp.stop_reason,
        StopReason::Other("content_filter".to_owned())
    );
}

#[test]
fn parse_response_rejects_non_json() {
    assert!(parse_response("not json at all").is_err());
}

// ---------------------------------------------------------------------------
// Base URL tests
// ---------------------------------------------------------------------------

#[test]
fn openai_provider_uses_default_base_url() {
    let provider = OpenAiProvider::new(
        "openai/gpt-5".to_owned(),
        "gpt-5".to_owned(),
        OpenAiAuth::ApiKey("test-key".to_owned()),
    );
    assert_eq!(provider.model_id(), "openai/gpt-5");
    assert!(provider.supports_tool_calling());
}

#[test]
fn openai_provider_accepts_custom_base_url() {
    let provider = OpenAiProvider::with_base_url(
        "deepseek/deepseek-chat".to_owned(),
        "deepseek-chat".to_owned(),
        OpenAiAuth::ApiKey("test-key".to_owned()),
        "https://api.deepseek.com/v1/chat/completions".to_owned(),
    );
    assert_eq!(provider.model_id(), "deepseek/deepseek-chat");
    assert!(provider.supports_tool_calling());
}
