/// Inference proxy — mediates all LLM communication (spec 6.3).
///
/// Routes inference requests based on data ceiling. Phase 1 targets
/// local Ollama only; cloud provider routing (Anthropic, OpenAI) with
/// label-based guards will be added in Phase 2.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::SecurityLabel;

/// Inference error types (spec 6.3).
#[derive(Debug, Error)]
pub enum InferenceError {
    /// HTTP request to provider failed.
    #[error("inference request failed: {0}")]
    RequestFailed(String),
    /// The requested model is not available.
    #[error("model not available: {0}")]
    ModelUnavailable(String),
    /// Token budget exceeded for this task.
    #[error("token limit exceeded")]
    TokenLimitExceeded,
    /// Data ceiling prevents routing to this provider.
    #[error("data ceiling {label:?} prevents cloud routing")]
    RoutingDenied { label: SecurityLabel },
}

/// Ollama generate request body.
#[derive(Debug, Serialize)]
struct OllamaRequest {
    model: String,
    prompt: String,
    stream: bool,
}

/// Ollama generate response body.
#[derive(Debug, Deserialize)]
struct OllamaResponse {
    response: String,
}

/// Trait for LLM inference (spec 6.3).
///
/// Allows swapping between real Ollama client and mock for testing.
#[async_trait]
pub trait InferenceProvider: Send + Sync {
    /// Generate a completion from the given model and prompt.
    async fn generate(
        &self,
        model: &str,
        prompt: &str,
        max_tokens: u32,
    ) -> Result<String, InferenceError>;
}

/// Inference proxy routing LLM calls via label-based rules (spec 6.3).
///
/// Phase 1: routes to local Ollama only.
/// Phase 2: adds cloud providers with label-based routing.
pub struct InferenceProxy {
    provider: Box<dyn InferenceProvider>,
}

impl InferenceProxy {
    /// Create an inference proxy with a local Ollama provider.
    pub fn local(ollama_base_url: &str) -> Self {
        Self {
            provider: Box::new(OllamaProvider {
                base_url: ollama_base_url.to_owned(),
                client: reqwest::Client::new(),
            }),
        }
    }

    /// Create an inference proxy with a custom provider (for testing).
    pub fn with_provider(provider: Box<dyn InferenceProvider>) -> Self {
        Self { provider }
    }

    /// Generate a completion, enforcing label-based routing (spec 6.3).
    ///
    /// Phase 1: always routes locally. Rejects `Secret` data.
    pub async fn generate(
        &self,
        model: &str,
        prompt: &str,
        max_tokens: u32,
        data_ceiling: SecurityLabel,
    ) -> Result<String, InferenceError> {
        // Secrets must never be sent to any LLM (spec 6.3).
        if data_ceiling == SecurityLabel::Secret {
            return Err(InferenceError::RoutingDenied {
                label: SecurityLabel::Secret,
            });
        }
        self.provider.generate(model, prompt, max_tokens).await
    }
}

/// Ollama HTTP provider (spec 6.3).
struct OllamaProvider {
    base_url: String,
    client: reqwest::Client,
}

#[async_trait]
impl InferenceProvider for OllamaProvider {
    async fn generate(
        &self,
        model: &str,
        prompt: &str,
        _max_tokens: u32,
    ) -> Result<String, InferenceError> {
        let url = format!("{}/api/generate", self.base_url);
        let body = OllamaRequest {
            model: model.to_owned(),
            prompt: prompt.to_owned(),
            stream: false,
        };

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| InferenceError::RequestFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("(body unreadable: {e})"));
            if status.as_u16() == 404 {
                return Err(InferenceError::ModelUnavailable(model.to_owned()));
            }
            return Err(InferenceError::RequestFailed(format!(
                "HTTP {status}: {text}"
            )));
        }

        let ollama_resp: OllamaResponse = resp
            .json()
            .await
            .map_err(|e| InferenceError::RequestFailed(e.to_string()))?;

        Ok(ollama_resp.response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock provider for unit testing without HTTP.
    struct MockProvider {
        response: String,
    }

    #[async_trait]
    impl InferenceProvider for MockProvider {
        async fn generate(
            &self,
            _model: &str,
            _prompt: &str,
            _max_tokens: u32,
        ) -> Result<String, InferenceError> {
            Ok(self.response.clone())
        }
    }

    struct FailingProvider;

    #[async_trait]
    impl InferenceProvider for FailingProvider {
        async fn generate(
            &self,
            model: &str,
            _prompt: &str,
            _max_tokens: u32,
        ) -> Result<String, InferenceError> {
            Err(InferenceError::ModelUnavailable(model.to_owned()))
        }
    }

    #[tokio::test]
    async fn test_generate_mock() {
        let proxy = InferenceProxy::with_provider(Box::new(MockProvider {
            response: "Hello, world!".to_owned(),
        }));
        let result = proxy
            .generate("llama3", "Say hello", 100, SecurityLabel::Internal)
            .await
            .expect("should succeed");
        assert_eq!(result, "Hello, world!");
    }

    #[tokio::test]
    async fn test_generate_rejects_secret() {
        let proxy = InferenceProxy::with_provider(Box::new(MockProvider {
            response: "nope".to_owned(),
        }));
        let result = proxy
            .generate("llama3", "prompt", 100, SecurityLabel::Secret)
            .await;
        assert!(matches!(result, Err(InferenceError::RoutingDenied { .. })));
    }

    #[tokio::test]
    async fn test_generate_allows_sensitive() {
        let proxy = InferenceProxy::with_provider(Box::new(MockProvider {
            response: "ok".to_owned(),
        }));
        let result = proxy
            .generate("llama3", "prompt", 100, SecurityLabel::Sensitive)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_failing_provider() {
        let proxy = InferenceProxy::with_provider(Box::new(FailingProvider));
        let result = proxy
            .generate("bad_model", "prompt", 100, SecurityLabel::Public)
            .await;
        assert!(matches!(result, Err(InferenceError::ModelUnavailable(_))));
    }
}
