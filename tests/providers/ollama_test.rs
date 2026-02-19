//! Ollama provider wire format tests.

use serde_json::json;
use wintermute::providers::ollama::{
    build_request, parse_response, OllamaProvider, DEFAULT_OLLAMA_URL,
};
use wintermute::providers::{
    CompletionRequest, ContentPart, Message, MessageContent, Role, StopReason,
};

fn simple_request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![Message {
            role: Role::User,
            content: MessageContent::Text("Hello".to_owned()),
        }],
        system: Some("You are helpful.".to_owned()),
        tools: vec![],
        max_tokens: Some(512),
        stop_sequences: vec![],
    }
}

#[test]
fn build_request_injects_system_message() {
    let req = build_request("qwen3:8b", &simple_request());
    assert_eq!(req.model, "qwen3:8b");
    assert_eq!(req.messages.len(), 2); // system + user
    assert_eq!(req.messages[0].role, "system");
    assert_eq!(req.messages[0].content, "You are helpful.");
    assert_eq!(req.messages[1].role, "user");
}

#[test]
fn build_request_no_system_when_absent() {
    let mut request = simple_request();
    request.system = None;
    let req = build_request("model", &request);
    assert_eq!(req.messages.len(), 1);
    assert_eq!(req.messages[0].role, "user");
}

#[test]
fn build_request_sets_options() {
    let req = build_request("model", &simple_request());
    assert!(req.options.is_some());
    let opts = req.options.expect("options should exist");
    assert_eq!(opts.num_predict, Some(512));
}

#[test]
fn build_request_maps_roles() {
    let request = CompletionRequest {
        messages: vec![
            Message {
                role: Role::System,
                content: MessageContent::Text("sys".to_owned()),
            },
            Message {
                role: Role::User,
                content: MessageContent::Text("usr".to_owned()),
            },
            Message {
                role: Role::Assistant,
                content: MessageContent::Text("ast".to_owned()),
            },
            Message {
                role: Role::Tool,
                content: MessageContent::Text("tl".to_owned()),
            },
        ],
        system: None,
        tools: vec![],
        max_tokens: None,
        stop_sequences: vec![],
    };
    let req = build_request("model", &request);
    assert_eq!(req.messages[0].role, "system");
    assert_eq!(req.messages[1].role, "user");
    assert_eq!(req.messages[2].role, "assistant");
    assert_eq!(req.messages[3].role, "tool");
}

#[test]
fn parse_response_text_only() {
    let body = json!({
        "message": {"role": "assistant", "content": "Hello!"},
        "model": "qwen3:8b",
        "prompt_eval_count": 10,
        "eval_count": 5
    });
    let resp = parse_response(&body.to_string()).expect("should parse");
    assert_eq!(resp.content.len(), 1);
    assert!(matches!(&resp.content[0], ContentPart::Text { text } if text == "Hello!"));
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(resp.usage.input_tokens, 10);
    assert_eq!(resp.usage.output_tokens, 5);
}

#[test]
fn parse_response_with_tool_calls() {
    let body = json!({
        "message": {
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "function": {
                    "name": "web_search",
                    "arguments": {"query": "rust"}
                }
            }]
        },
        "model": "qwen3:8b",
        "prompt_eval_count": 20,
        "eval_count": 10
    });
    let resp = parse_response(&body.to_string()).expect("should parse");
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    assert!(resp
        .content
        .iter()
        .any(|p| matches!(p, ContentPart::ToolUse { name, .. } if name == "web_search")));
}

#[test]
fn parse_response_no_usage() {
    let body = json!({
        "message": {"role": "assistant", "content": "Hi"},
        "model": "m"
    });
    let resp = parse_response(&body.to_string()).expect("should parse");
    assert_eq!(resp.usage.input_tokens, 0);
    assert_eq!(resp.usage.output_tokens, 0);
}

#[test]
fn parse_response_invalid_json() {
    let result = parse_response("not json");
    assert!(result.is_err());
}

#[test]
fn ollama_provider_default_url() {
    assert_eq!(DEFAULT_OLLAMA_URL, "http://127.0.0.1:11434");
}

#[test]
fn ollama_provider_pub_fields() {
    let provider = OllamaProvider::new("ollama/qwen3:8b".to_owned(), "qwen3:8b".to_owned());
    assert_eq!(provider.model, "qwen3:8b");
    assert_eq!(provider.base_url, DEFAULT_OLLAMA_URL);
}
