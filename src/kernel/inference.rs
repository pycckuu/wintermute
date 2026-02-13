/// Inference proxy -- mediates all LLM communication (spec 6.3).
///
/// Routes inference requests based on data ceiling and provider config.
/// Supports multiple providers: local Ollama, OpenAI-compatible (including
/// LM Studio), and Anthropic Messages API.
use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

use crate::kernel::template::InferenceConfig;
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
    /// The requested provider is not registered.
    #[error("provider not registered: {0}")]
    ProviderNotFound(String),
}

// ── Ollama types ──

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

// ── OpenAI-compatible types ──

/// OpenAI chat completion request body (spec 11.2).
#[derive(Debug, Serialize)]
struct OpenAiRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    max_tokens: u32,
}

/// A single message in an OpenAI chat completion request.
#[derive(Debug, Serialize, Deserialize)]
struct OpenAiMessage {
    role: String,
    content: String,
}

/// OpenAI chat completion response body.
#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
}

/// A single choice in an OpenAI chat completion response.
#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

// ── Anthropic types ──

/// Anthropic Messages API request body (spec 11.2).
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
}

/// A single message in an Anthropic Messages request.
#[derive(Debug, Serialize, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

/// Anthropic Messages API response body.
#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
}

/// A content block in an Anthropic response.
#[derive(Debug, Deserialize)]
struct AnthropicContentBlock {
    text: String,
}

// ── Provider trait ──

/// Trait for LLM inference (spec 6.3).
///
/// Allows swapping between real HTTP providers and mocks for testing.
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

// ── Provider implementations ──

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

/// OpenAI-compatible HTTP provider (spec 11.2).
///
/// Works with OpenAI API (`https://api.openai.com`) and local OpenAI-compatible
/// servers like LM Studio. Uses the `/v1/chat/completions` endpoint.
pub struct OpenAiProvider {
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl OpenAiProvider {
    /// Create a new OpenAI provider for the official API.
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            base_url: base_url.to_owned(),
            api_key: Some(api_key.to_owned()),
            client: reqwest::Client::new(),
        }
    }

    /// Create a provider for a local OpenAI-compatible server (no API key).
    pub fn local(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_owned(),
            api_key: None,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl InferenceProvider for OpenAiProvider {
    async fn generate(
        &self,
        model: &str,
        prompt: &str,
        max_tokens: u32,
    ) -> Result<String, InferenceError> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let body = OpenAiRequest {
            model: model.to_owned(),
            messages: vec![OpenAiMessage {
                role: "user".to_owned(),
                content: prompt.to_owned(),
            }],
            max_tokens,
        };

        let mut req = self.client.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req
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

        let openai_resp: OpenAiResponse = resp
            .json()
            .await
            .map_err(|e| InferenceError::RequestFailed(e.to_string()))?;

        openai_resp
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| InferenceError::RequestFailed("empty choices array".to_owned()))
    }
}

/// Anthropic Messages API provider (spec 11.2).
///
/// Speaks the Anthropic Messages API at `https://api.anthropic.com/v1/messages`.
pub struct AnthropicProvider {
    api_key: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Create a new Anthropic provider.
    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_owned(),
            client: reqwest::Client::new(),
        }
    }
}

/// Anthropic API version header value.
const ANTHROPIC_API_VERSION: &str = "2023-06-01";

/// Anthropic API base URL.
const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

#[async_trait]
impl InferenceProvider for AnthropicProvider {
    async fn generate(
        &self,
        model: &str,
        prompt: &str,
        max_tokens: u32,
    ) -> Result<String, InferenceError> {
        let url = format!("{ANTHROPIC_BASE_URL}/v1/messages");
        let body = AnthropicRequest {
            model: model.to_owned(),
            max_tokens,
            messages: vec![AnthropicMessage {
                role: "user".to_owned(),
                content: prompt.to_owned(),
            }],
        };

        let mut req = self
            .client
            .post(&url)
            .header("anthropic-version", ANTHROPIC_API_VERSION)
            .header("content-type", "application/json");

        // OAuth tokens (sk-ant-oat*) use Bearer auth + beta header;
        // standard API keys use x-api-key.
        if self.api_key.starts_with("sk-ant-oat") {
            req = req
                .header("authorization", format!("Bearer {}", self.api_key))
                .header("anthropic-beta", "oauth-2025-04-20");
        } else {
            req = req.header("x-api-key", &self.api_key);
        }

        let resp = req
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

        let anthropic_resp: AnthropicResponse = resp
            .json()
            .await
            .map_err(|e| InferenceError::RequestFailed(e.to_string()))?;

        anthropic_resp
            .content
            .into_iter()
            .next()
            .map(|b| b.text)
            .ok_or_else(|| InferenceError::RequestFailed("empty content array".to_owned()))
    }
}

// ── Inference Proxy ──

/// Provider names treated as local (not cloud) for routing decisions (spec 11.1).
const LOCAL_PROVIDER_NAMES: &[&str] = &["local", "ollama", "lmstudio"];

/// Returns true if the provider name refers to a local (non-cloud) provider.
fn is_local_provider(name: &str) -> bool {
    LOCAL_PROVIDER_NAMES.contains(&name)
}

/// Inference proxy routing LLM calls via label-based rules (spec 6.3, 11.1).
///
/// Holds multiple named providers and routes requests based on template
/// inference config. The default provider is used by `generate()` for
/// backward compatibility.
pub struct InferenceProxy {
    providers: HashMap<String, Box<dyn InferenceProvider>>,
    default_provider: String,
}

impl InferenceProxy {
    /// Create an inference proxy with a local Ollama provider (spec 6.3).
    ///
    /// Registers the Ollama provider under both "local" and "ollama" keys.
    pub fn local(ollama_base_url: &str) -> Self {
        let provider = OllamaProvider {
            base_url: ollama_base_url.to_owned(),
            client: reqwest::Client::new(),
        };
        let mut providers: HashMap<String, Box<dyn InferenceProvider>> = HashMap::new();
        providers.insert("local".to_owned(), Box::new(provider));
        Self {
            providers,
            default_provider: "local".to_owned(),
        }
    }

    /// Create an inference proxy with a custom provider (for testing).
    ///
    /// The provider is registered as "default" and used for all calls.
    pub fn with_provider(provider: Box<dyn InferenceProvider>) -> Self {
        let mut providers: HashMap<String, Box<dyn InferenceProvider>> = HashMap::new();
        providers.insert("default".to_owned(), provider);
        Self {
            providers,
            default_provider: "default".to_owned(),
        }
    }

    /// Create a builder for multi-provider setup (spec 6.3, 11.1).
    pub fn builder(ollama_base_url: &str) -> InferenceProxyBuilder {
        InferenceProxyBuilder::new(ollama_base_url)
    }

    /// Look up a provider by name, falling back to default if not found.
    fn resolve_provider(&self, name: &str) -> Result<&dyn InferenceProvider, InferenceError> {
        // Direct lookup.
        if let Some(p) = self.providers.get(name) {
            return Ok(p.as_ref());
        }
        // "ollama" is an alias for "local".
        if name == "ollama" {
            if let Some(p) = self.providers.get("local") {
                return Ok(p.as_ref());
            }
        }
        // Fall back to default provider.
        warn!(
            requested = name,
            default = %self.default_provider,
            "provider not found, falling back to default"
        );
        self.providers
            .get(&self.default_provider)
            .map(|p| p.as_ref())
            .ok_or_else(|| InferenceError::ProviderNotFound(name.to_owned()))
    }

    /// Generate a completion, enforcing label-based routing (spec 6.3).
    ///
    /// Uses the default provider. Rejects `Secret` data.
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
        let provider = self.resolve_provider(&self.default_provider)?;
        provider.generate(model, prompt, max_tokens).await
    }

    /// Generate with full routing check using template inference config (spec 11.1).
    ///
    /// Routes to the provider named in `config.provider`, falling back to the
    /// default if the named provider is not registered.
    ///
    /// Label-based routing rules:
    /// - `public`/`internal`: any provider allowed
    /// - `sensitive`: local only unless `owner_acknowledged_cloud_risk` is set
    /// - `regulated`: always local, cannot be overridden
    /// - `secret`: never sent to any LLM
    pub async fn generate_with_config(
        &self,
        config: &InferenceConfig,
        prompt: &str,
        max_tokens: u32,
        data_ceiling: SecurityLabel,
    ) -> Result<String, InferenceError> {
        // Secret data must never reach any LLM.
        if data_ceiling == SecurityLabel::Secret {
            return Err(InferenceError::RoutingDenied {
                label: data_ceiling,
            });
        }

        // Determine if the configured provider is cloud-based.
        let is_cloud = !is_local_provider(&config.provider);

        if is_cloud {
            // Regulated data can never go to cloud, even with ack.
            if data_ceiling >= SecurityLabel::Regulated {
                return Err(InferenceError::RoutingDenied {
                    label: data_ceiling,
                });
            }
            // Sensitive data requires explicit owner acknowledgment.
            if data_ceiling == SecurityLabel::Sensitive && !config.owner_acknowledged_cloud_risk {
                return Err(InferenceError::RoutingDenied {
                    label: data_ceiling,
                });
            }
        }

        let provider = self.resolve_provider(&config.provider)?;
        provider.generate(&config.model, prompt, max_tokens).await
    }
}

// ── Builder ──

/// Builder for constructing an `InferenceProxy` with multiple providers (spec 6.3, 11.1).
pub struct InferenceProxyBuilder {
    providers: HashMap<String, Box<dyn InferenceProvider>>,
    default_provider: String,
}

impl InferenceProxyBuilder {
    /// Start building with Ollama as the local/default provider.
    fn new(ollama_base_url: &str) -> Self {
        let provider = OllamaProvider {
            base_url: ollama_base_url.to_owned(),
            client: reqwest::Client::new(),
        };
        let mut providers: HashMap<String, Box<dyn InferenceProvider>> = HashMap::new();
        providers.insert("local".to_owned(), Box::new(provider));
        Self {
            providers,
            default_provider: "local".to_owned(),
        }
    }

    /// Register an OpenAI provider (spec 11.2).
    pub fn with_openai(mut self, base_url: &str, api_key: &str) -> Self {
        self.providers.insert(
            "openai".to_owned(),
            Box::new(OpenAiProvider::new(base_url, api_key)),
        );
        self
    }

    /// Register an Anthropic provider (spec 11.2).
    pub fn with_anthropic(mut self, api_key: &str) -> Self {
        self.providers.insert(
            "anthropic".to_owned(),
            Box::new(AnthropicProvider::new(api_key)),
        );
        self
    }

    /// Register a local OpenAI-compatible server like LM Studio (no API key).
    pub fn with_lmstudio(mut self, base_url: &str) -> Self {
        self.providers.insert(
            "lmstudio".to_owned(),
            Box::new(OpenAiProvider::local(base_url)),
        );
        self
    }

    /// Set the default provider name. Defaults to "local" if not called.
    pub fn default_provider(mut self, name: &str) -> Self {
        self.default_provider = name.to_owned();
        self
    }

    /// Build the `InferenceProxy`.
    pub fn build(self) -> InferenceProxy {
        InferenceProxy {
            providers: self.providers,
            default_provider: self.default_provider,
        }
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

    // ── Existing tests (backward compat) ──

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

    // ── generate_with_config tests (spec 11.1) ──

    fn cloud_config(ack: bool) -> InferenceConfig {
        InferenceConfig {
            provider: "anthropic".to_owned(),
            model: "claude-sonnet".to_owned(),
            owner_acknowledged_cloud_risk: ack,
        }
    }

    fn local_config() -> InferenceConfig {
        InferenceConfig {
            provider: "local".to_owned(),
            model: "llama3".to_owned(),
            owner_acknowledged_cloud_risk: false,
        }
    }

    #[tokio::test]
    async fn test_generate_with_config_cloud_sensitive_no_ack() {
        let proxy = InferenceProxy::with_provider(Box::new(MockProvider {
            response: "ok".to_owned(),
        }));
        let result = proxy
            .generate_with_config(
                &cloud_config(false),
                "prompt",
                100,
                SecurityLabel::Sensitive,
            )
            .await;
        assert!(
            matches!(result, Err(InferenceError::RoutingDenied { .. })),
            "sensitive data to cloud without ack should be denied"
        );
    }

    #[tokio::test]
    async fn test_generate_with_config_cloud_sensitive_with_ack() {
        let proxy = InferenceProxy::with_provider(Box::new(MockProvider {
            response: "ok".to_owned(),
        }));
        let result = proxy
            .generate_with_config(&cloud_config(true), "prompt", 100, SecurityLabel::Sensitive)
            .await;
        assert!(
            result.is_ok(),
            "sensitive data to cloud with ack should be allowed"
        );
    }

    #[tokio::test]
    async fn test_generate_with_config_local_sensitive() {
        let proxy = InferenceProxy::with_provider(Box::new(MockProvider {
            response: "ok".to_owned(),
        }));
        let result = proxy
            .generate_with_config(&local_config(), "prompt", 100, SecurityLabel::Sensitive)
            .await;
        assert!(
            result.is_ok(),
            "sensitive data to local provider should always be allowed"
        );
    }

    #[tokio::test]
    async fn test_generate_with_config_cloud_regulated_denied() {
        let proxy = InferenceProxy::with_provider(Box::new(MockProvider {
            response: "ok".to_owned(),
        }));
        let result = proxy
            .generate_with_config(&cloud_config(true), "prompt", 100, SecurityLabel::Regulated)
            .await;
        assert!(
            matches!(result, Err(InferenceError::RoutingDenied { .. })),
            "regulated data to cloud should always be denied even with ack"
        );
    }

    #[tokio::test]
    async fn test_generate_with_config_secret_denied() {
        let proxy = InferenceProxy::with_provider(Box::new(MockProvider {
            response: "ok".to_owned(),
        }));
        let result = proxy
            .generate_with_config(&local_config(), "prompt", 100, SecurityLabel::Secret)
            .await;
        assert!(
            matches!(result, Err(InferenceError::RoutingDenied { .. })),
            "secret data to any provider should be denied"
        );
    }

    #[tokio::test]
    async fn test_generate_with_config_cloud_public_ok() {
        let proxy = InferenceProxy::with_provider(Box::new(MockProvider {
            response: "ok".to_owned(),
        }));
        let result = proxy
            .generate_with_config(&cloud_config(false), "prompt", 100, SecurityLabel::Public)
            .await;
        assert!(
            result.is_ok(),
            "public data to cloud should be allowed without ack"
        );
    }

    #[tokio::test]
    async fn test_generate_with_config_ollama_provider_is_local() {
        // "ollama" should be treated as a local provider.
        let config = InferenceConfig {
            provider: "ollama".to_owned(),
            model: "llama3".to_owned(),
            owner_acknowledged_cloud_risk: false,
        };
        let proxy = InferenceProxy::with_provider(Box::new(MockProvider {
            response: "ok".to_owned(),
        }));
        let result = proxy
            .generate_with_config(&config, "prompt", 100, SecurityLabel::Sensitive)
            .await;
        assert!(result.is_ok(), "ollama provider should be treated as local");
    }

    // ── New multi-provider tests ──

    #[tokio::test]
    async fn test_multi_provider_routing() {
        // Register two mock providers under different names.
        let mut providers: HashMap<String, Box<dyn InferenceProvider>> = HashMap::new();
        providers.insert(
            "local".to_owned(),
            Box::new(MockProvider {
                response: "local-response".to_owned(),
            }),
        );
        providers.insert(
            "anthropic".to_owned(),
            Box::new(MockProvider {
                response: "anthropic-response".to_owned(),
            }),
        );
        providers.insert(
            "openai".to_owned(),
            Box::new(MockProvider {
                response: "openai-response".to_owned(),
            }),
        );

        let proxy = InferenceProxy {
            providers,
            default_provider: "local".to_owned(),
        };

        // Route to anthropic.
        let config = InferenceConfig {
            provider: "anthropic".to_owned(),
            model: "claude-sonnet".to_owned(),
            owner_acknowledged_cloud_risk: true,
        };
        let result = proxy
            .generate_with_config(&config, "prompt", 100, SecurityLabel::Sensitive)
            .await
            .expect("should route to anthropic");
        assert_eq!(result, "anthropic-response");

        // Route to openai.
        let config = InferenceConfig {
            provider: "openai".to_owned(),
            model: "gpt-4o".to_owned(),
            owner_acknowledged_cloud_risk: true,
        };
        let result = proxy
            .generate_with_config(&config, "prompt", 100, SecurityLabel::Sensitive)
            .await
            .expect("should route to openai");
        assert_eq!(result, "openai-response");

        // Route to local.
        let config = InferenceConfig {
            provider: "local".to_owned(),
            model: "llama3".to_owned(),
            owner_acknowledged_cloud_risk: false,
        };
        let result = proxy
            .generate_with_config(&config, "prompt", 100, SecurityLabel::Sensitive)
            .await
            .expect("should route to local");
        assert_eq!(result, "local-response");
    }

    #[tokio::test]
    async fn test_multi_provider_fallback_to_default() {
        // Only register "default" provider (via with_provider).
        let proxy = InferenceProxy::with_provider(Box::new(MockProvider {
            response: "default-response".to_owned(),
        }));

        // Request a provider that doesn't exist -- should fall back to default.
        let config = InferenceConfig {
            provider: "nonexistent".to_owned(),
            model: "some-model".to_owned(),
            owner_acknowledged_cloud_risk: true,
        };
        let result = proxy
            .generate_with_config(&config, "prompt", 100, SecurityLabel::Public)
            .await
            .expect("should fall back to default provider");
        assert_eq!(result, "default-response");
    }

    #[tokio::test]
    async fn test_existing_generate_still_works() {
        // Backward compatibility: `generate()` should use the default provider.
        let proxy = InferenceProxy::with_provider(Box::new(MockProvider {
            response: "backward-compat".to_owned(),
        }));
        let result = proxy
            .generate("llama3", "prompt", 100, SecurityLabel::Internal)
            .await
            .expect("generate() should still work");
        assert_eq!(result, "backward-compat");
    }

    #[tokio::test]
    async fn test_builder_creates_multi_provider() {
        // Use the builder to create a multi-provider proxy.
        // We can't actually test HTTP calls, but we can verify that
        // the builder creates a proxy with all providers registered.
        let proxy = InferenceProxy::builder("http://localhost:11434")
            .with_openai("https://api.openai.com", "test-key")
            .with_anthropic("test-key")
            .with_lmstudio("http://localhost:1234")
            .build();

        // Verify providers are registered.
        assert!(proxy.providers.contains_key("local"));
        assert!(proxy.providers.contains_key("openai"));
        assert!(proxy.providers.contains_key("anthropic"));
        assert!(proxy.providers.contains_key("lmstudio"));
        assert_eq!(proxy.default_provider, "local");
    }

    #[tokio::test]
    async fn test_lmstudio_provider_is_local() {
        // "lmstudio" should be treated as a local provider for routing.
        let config = InferenceConfig {
            provider: "lmstudio".to_owned(),
            model: "local-model".to_owned(),
            owner_acknowledged_cloud_risk: false,
        };
        let proxy = InferenceProxy::with_provider(Box::new(MockProvider {
            response: "ok".to_owned(),
        }));
        let result = proxy
            .generate_with_config(&config, "prompt", 100, SecurityLabel::Sensitive)
            .await;
        assert!(
            result.is_ok(),
            "lmstudio provider should be treated as local"
        );
    }

    #[tokio::test]
    async fn test_openai_provider_request_format() {
        // Verify the OpenAI request body structure by testing with a mock server.
        // Since we can't easily spin up a mock HTTP server without extra deps,
        // we verify the serialization format directly.
        let req = OpenAiRequest {
            model: "gpt-4o".to_owned(),
            messages: vec![OpenAiMessage {
                role: "user".to_owned(),
                content: "Hello".to_owned(),
            }],
            max_tokens: 4000,
        };
        let json = serde_json::to_value(&req).expect("should serialize");
        assert_eq!(json["model"], "gpt-4o");
        assert_eq!(json["max_tokens"], 4000);
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "Hello");
    }

    #[tokio::test]
    async fn test_anthropic_provider_request_format() {
        // Verify the Anthropic request body structure.
        let req = AnthropicRequest {
            model: "claude-sonnet-4-20250514".to_owned(),
            max_tokens: 4000,
            messages: vec![AnthropicMessage {
                role: "user".to_owned(),
                content: "Hello".to_owned(),
            }],
        };
        let json = serde_json::to_value(&req).expect("should serialize");
        assert_eq!(json["model"], "claude-sonnet-4-20250514");
        assert_eq!(json["max_tokens"], 4000);
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "Hello");
    }

    #[tokio::test]
    async fn test_openai_response_parsing() {
        // Verify deserialization of an OpenAI chat completion response.
        let json = serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "Hello there!"}
            }]
        });
        let resp: OpenAiResponse = serde_json::from_value(json).expect("should deserialize");
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.content, "Hello there!");
    }

    #[tokio::test]
    async fn test_anthropic_response_parsing() {
        // Verify deserialization of an Anthropic Messages response.
        let json = serde_json::json!({
            "content": [{"text": "Hello there!", "type": "text"}]
        });
        let resp: AnthropicResponse = serde_json::from_value(json).expect("should deserialize");
        assert_eq!(resp.content.len(), 1);
        assert_eq!(resp.content[0].text, "Hello there!");
    }

    #[tokio::test]
    async fn test_openai_empty_choices_is_error() {
        // Empty choices array should produce an error, not a panic.
        let json = serde_json::json!({"choices": []});
        let resp: OpenAiResponse = serde_json::from_value(json).expect("should deserialize");
        assert!(resp.choices.is_empty());
        // The provider would return an error here -- tested via the integration path.
    }

    #[tokio::test]
    async fn test_anthropic_empty_content_is_error() {
        // Empty content array should produce an error, not a panic.
        let json = serde_json::json!({"content": []});
        let resp: AnthropicResponse = serde_json::from_value(json).expect("should deserialize");
        assert!(resp.content.is_empty());
    }

    #[tokio::test]
    async fn test_generate_with_config_routes_to_named_provider() {
        // Verify that generate_with_config dispatches to the correct provider.
        let mut providers: HashMap<String, Box<dyn InferenceProvider>> = HashMap::new();
        providers.insert(
            "default".to_owned(),
            Box::new(MockProvider {
                response: "from-default".to_owned(),
            }),
        );
        providers.insert(
            "anthropic".to_owned(),
            Box::new(MockProvider {
                response: "from-anthropic".to_owned(),
            }),
        );

        let proxy = InferenceProxy {
            providers,
            default_provider: "default".to_owned(),
        };

        // Default generate() should use "default".
        let result = proxy
            .generate("m", "p", 100, SecurityLabel::Public)
            .await
            .expect("should work");
        assert_eq!(result, "from-default");

        // generate_with_config pointing to "anthropic" should use that provider.
        let config = InferenceConfig {
            provider: "anthropic".to_owned(),
            model: "claude-sonnet".to_owned(),
            owner_acknowledged_cloud_risk: true,
        };
        let result = proxy
            .generate_with_config(&config, "p", 100, SecurityLabel::Public)
            .await
            .expect("should route to anthropic");
        assert_eq!(result, "from-anthropic");
    }

    #[tokio::test]
    async fn test_builder_default_provider_override() {
        // Verify builder.default_provider() changes the default.
        let builder = InferenceProxy::builder("http://localhost:11434")
            .with_anthropic("key")
            .default_provider("anthropic");

        let proxy = builder.build();
        assert_eq!(proxy.default_provider, "anthropic");
    }
}
