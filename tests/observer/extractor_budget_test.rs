//! Tests for observer extraction budget gate and redaction chokepoint.

use std::sync::Arc;

use async_trait::async_trait;

use wintermute::agent::budget::DailyBudget;
use wintermute::executor::redactor::Redactor;
use wintermute::observer::extractor::{extract, user_message};
use wintermute::providers::router::ModelRouter;
use wintermute::providers::{
    CompletionRequest, CompletionResponse, ContentPart, LlmProvider, ProviderError, StopReason,
    UsageStats,
};

/// A mock LLM provider that returns a canned response.
struct MockProvider {
    response_text: String,
}

#[async_trait]
impl LlmProvider for MockProvider {
    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        Ok(CompletionResponse {
            content: vec![ContentPart::Text {
                text: self.response_text.clone(),
            }],
            stop_reason: StopReason::EndTurn,
            usage: UsageStats {
                input_tokens: 100,
                output_tokens: 50,
            },
            model: "mock".to_owned(),
        })
    }

    fn supports_tool_calling(&self) -> bool {
        false
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn model_id(&self) -> &str {
        "mock"
    }
}

fn mock_router(response: &str) -> ModelRouter {
    let provider = Arc::new(MockProvider {
        response_text: response.to_owned(),
    });
    ModelRouter::for_testing("mock".to_owned(), provider)
}

#[tokio::test]
async fn extract_rejects_when_budget_exhausted() {
    let router = mock_router("[]");
    let redactor = Redactor::new(vec![]);
    // Budget of 1 token — any extraction call (estimated 500) will exceed it.
    let budget = DailyBudget::new(1);

    let messages = vec![user_message("hello world")];
    let result = extract(&messages, &router, &redactor, &budget).await;

    let err = result.expect_err("extract should fail when budget exceeded");
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("budget"),
        "error should mention budget: {err_msg}"
    );
}

#[tokio::test]
async fn extract_redacts_secrets_from_llm_output() {
    let secret = "sk-ant-secret-key-12345";
    let response_json =
        format!(r#"[{{"kind": "fact", "content": "API key is {secret}", "confidence": 0.9}}]"#);
    let router = mock_router(&response_json);
    // The redactor knows this secret and should mask it.
    let redactor = Redactor::new(vec![secret.to_owned()]);
    let budget = DailyBudget::new(100_000);

    let messages = vec![user_message("here is my API key")];
    let result = extract(&messages, &router, &redactor, &budget)
        .await
        .expect("extract should succeed");

    // The extraction content should NOT contain the raw secret.
    for extraction in &result {
        assert!(
            !extraction.content.contains(secret),
            "extraction content should not contain raw secret: {}",
            extraction.content
        );
    }
}
