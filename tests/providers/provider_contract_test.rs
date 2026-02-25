//! Provider capability contract tests.

use wintermute::credentials::{AnthropicAuth, OpenAiAuth};
use wintermute::providers::anthropic::AnthropicProvider;
use wintermute::providers::ollama::OllamaProvider;
use wintermute::providers::openai::OpenAiProvider;
use wintermute::providers::LlmProvider;

#[test]
fn anthropic_provider_reports_capabilities_and_model_id() {
    let provider = AnthropicProvider::new(
        "anthropic/claude-sonnet-4-5-20250929".to_owned(),
        "claude-sonnet-4-5-20250929".to_owned(),
        AnthropicAuth::ApiKey("test-api-key".to_owned()),
    );
    assert!(provider.supports_tool_calling());
    assert!(provider.supports_streaming());
    assert_eq!(provider.model_id(), "anthropic/claude-sonnet-4-5-20250929");
}

#[test]
fn ollama_provider_reports_capabilities_and_model_id() {
    let provider = OllamaProvider::new("ollama/qwen3:8b".to_owned(), "qwen3:8b".to_owned());
    assert!(provider.supports_tool_calling());
    assert!(provider.supports_streaming());
    assert_eq!(provider.model_id(), "ollama/qwen3:8b");
}

#[test]
fn openai_provider_reports_capabilities_and_model_id() {
    let provider = OpenAiProvider::new(
        "openai/gpt-5".to_owned(),
        "gpt-5".to_owned(),
        OpenAiAuth::ApiKey("test-api-key".to_owned()),
    );
    assert!(provider.supports_tool_calling());
    assert!(provider.supports_streaming());
    assert_eq!(provider.model_id(), "openai/gpt-5");
}
