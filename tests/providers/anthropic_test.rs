//! Anthropic provider wire format tests.

use serde_json::json;
use wintermute::providers::anthropic::{build_request, parse_response};
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
        max_tokens: Some(1024),
        stop_sequences: vec![],
    }
}

#[test]
fn build_request_sets_model_and_system() {
    let req = build_request("claude-sonnet", &simple_request());
    assert_eq!(req.model, "claude-sonnet");
    assert_eq!(req.system, Some("You are helpful.".to_owned()));
    assert_eq!(req.max_tokens, 1024);
}

#[test]
fn build_request_maps_roles_correctly() {
    let request = CompletionRequest {
        messages: vec![
            Message {
                role: Role::User,
                content: MessageContent::Text("hi".to_owned()),
            },
            Message {
                role: Role::Assistant,
                content: MessageContent::Text("hello".to_owned()),
            },
            Message {
                role: Role::Tool,
                content: MessageContent::Text("result".to_owned()),
            },
        ],
        system: None,
        tools: vec![],
        max_tokens: None,
        stop_sequences: vec![],
    };
    let req = build_request("model", &request);
    assert_eq!(req.messages[0].role, "user");
    assert_eq!(req.messages[1].role, "assistant");
    assert_eq!(req.messages[2].role, "user"); // Tool maps to user
}

#[test]
fn build_request_default_max_tokens() {
    let mut request = simple_request();
    request.max_tokens = None;
    let req = build_request("model", &request);
    assert_eq!(req.max_tokens, 4096);
}

#[test]
fn parse_response_text_only() {
    let body = json!({
        "content": [{"type": "text", "text": "Hello world"}],
        "model": "claude-sonnet-4-5-20250929",
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 10, "output_tokens": 5}
    });
    let resp = parse_response(&body.to_string()).expect("should parse");
    assert_eq!(resp.content.len(), 1);
    assert!(matches!(&resp.content[0], ContentPart::Text { text } if text == "Hello world"));
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(resp.usage.input_tokens, 10);
    assert_eq!(resp.usage.output_tokens, 5);
    assert_eq!(resp.model, "claude-sonnet-4-5-20250929");
}

#[test]
fn parse_response_with_tool_use() {
    let body = json!({
        "content": [
            {"type": "text", "text": "Let me search."},
            {"type": "tool_use", "id": "call_1", "name": "web_search", "input": {"query": "rust"}}
        ],
        "model": "claude-sonnet",
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 20, "output_tokens": 15}
    });
    let resp = parse_response(&body.to_string()).expect("should parse");
    assert_eq!(resp.content.len(), 2);
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    match &resp.content[1] {
        ContentPart::ToolUse { id, name, input } => {
            assert_eq!(id, "call_1");
            assert_eq!(name, "web_search");
            assert_eq!(input["query"], "rust");
        }
        other => panic!("expected ToolUse, got {other:?}"),
    }
}

#[test]
fn parse_response_stop_reason_variants() {
    for (reason_str, expected) in [
        ("end_turn", StopReason::EndTurn),
        ("tool_use", StopReason::ToolUse),
        ("max_tokens", StopReason::MaxTokens),
        ("stop_sequence", StopReason::StopSequence),
    ] {
        let body = json!({
            "content": [{"type": "text", "text": "x"}],
            "model": "m",
            "stop_reason": reason_str,
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let resp = parse_response(&body.to_string()).expect("should parse");
        assert_eq!(resp.stop_reason, expected);
    }
}

#[test]
fn parse_response_invalid_json() {
    let result = parse_response("not json");
    assert!(result.is_err());
}
