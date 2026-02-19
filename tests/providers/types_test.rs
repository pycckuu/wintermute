//! Tests for provider types and utility functions.

use wintermute::providers::{
    parse_provider_string, ContentPart, Message, MessageContent, ProviderError, Role, UsageStats,
};

// ---------------------------------------------------------------------------
// parse_provider_string
// ---------------------------------------------------------------------------

#[test]
fn parse_provider_string_valid() {
    let (provider, model) = parse_provider_string("anthropic/claude-sonnet").expect("should parse");
    assert_eq!(provider, "anthropic");
    assert_eq!(model, "claude-sonnet");
}

#[test]
fn parse_provider_string_no_slash() {
    let result = parse_provider_string("no-slash");
    assert!(result.is_err());
}

#[test]
fn parse_provider_string_empty_provider() {
    let result = parse_provider_string("/model");
    assert!(result.is_err());
}

#[test]
fn parse_provider_string_empty_model() {
    let result = parse_provider_string("provider/");
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// MessageContent
// ---------------------------------------------------------------------------

#[test]
fn message_content_text_extracts_text() {
    let content = MessageContent::Text("hello".to_owned());
    assert_eq!(content.text(), "hello");
}

#[test]
fn message_content_parts_extracts_text() {
    let content = MessageContent::Parts(vec![
        ContentPart::Text {
            text: "hello ".to_owned(),
        },
        ContentPart::ToolUse {
            id: "1".to_owned(),
            name: "tool".to_owned(),
            input: serde_json::json!({}),
        },
        ContentPart::Text {
            text: "world".to_owned(),
        },
    ]);
    assert_eq!(content.text(), "hello world");
}

#[test]
fn message_content_empty_parts() {
    let content = MessageContent::Parts(vec![]);
    assert_eq!(content.text(), "");
}

// ---------------------------------------------------------------------------
// Role
// ---------------------------------------------------------------------------

#[test]
fn role_serialization_roundtrip() {
    let roles = [Role::System, Role::User, Role::Assistant, Role::Tool];
    for role in &roles {
        let json = serde_json::to_string(role).expect("should serialize");
        let parsed: Role = serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(parsed, *role);
    }
}

// ---------------------------------------------------------------------------
// UsageStats
// ---------------------------------------------------------------------------

#[test]
fn usage_stats_hash_and_eq() {
    let a = UsageStats {
        input_tokens: 10,
        output_tokens: 5,
    };
    let b = UsageStats {
        input_tokens: 10,
        output_tokens: 5,
    };
    assert_eq!(a, b);
}

// ---------------------------------------------------------------------------
// Message
// ---------------------------------------------------------------------------

#[test]
fn message_with_text_content() {
    let msg = Message {
        role: Role::User,
        content: MessageContent::Text("test".to_owned()),
    };
    assert_eq!(msg.content.text(), "test");
    assert_eq!(msg.role, Role::User);
}

// ---------------------------------------------------------------------------
// ProviderError::is_context_overflow
// ---------------------------------------------------------------------------

#[test]
fn provider_error_context_overflow_detects_anthropic_style() {
    let err = ProviderError::HttpStatus {
        status: 400,
        body: "input_length and max_tokens exceed context limit: 199211+20000 > 2000000".to_owned(),
    };
    assert!(err.is_context_overflow());
}

#[test]
fn provider_error_context_overflow_detects_context_length_exceeded() {
    let err = ProviderError::HttpStatus {
        status: 400,
        body: "context_length_exceeded: input too long".to_owned(),
    };
    assert!(err.is_context_overflow());
}

#[test]
fn provider_error_context_overflow_detects_input_too_long() {
    let err = ProviderError::HttpStatus {
        status: 400,
        body: "Input is too long for requested model.".to_owned(),
    };
    assert!(err.is_context_overflow());
}

#[test]
fn provider_error_context_overflow_rejects_non_overflow() {
    let err = ProviderError::HttpStatus {
        status: 429,
        body: "Rate limit exceeded".to_owned(),
    };
    assert!(!err.is_context_overflow());
}
