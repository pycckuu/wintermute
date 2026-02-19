//! Embedding generation trait and Ollama implementation.
//!
//! The [`Embedder`] trait abstracts over embedding providers. The default
//! implementation [`OllamaEmbedder`] calls the Ollama `/api/embed` endpoint
//! for local embedding generation.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Core embedding generation interface.
///
/// All implementations must be `Send + Sync` to allow shared use across
/// async task boundaries.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Generate an embedding vector for the given text.
    ///
    /// # Errors
    ///
    /// Returns an error if the embedding provider is unreachable or the
    /// request fails.
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedderError>;

    /// Returns the dimensionality of the embedding vectors produced.
    fn dimensions(&self) -> usize;
}

/// Errors from embedding generation.
#[derive(Debug, thiserror::Error)]
pub enum EmbedderError {
    /// HTTP transport failure.
    #[error("embedder request failed: {0}")]
    Request(#[from] reqwest::Error),

    /// Response did not match expected format.
    #[error("embedder response parse error: {0}")]
    Parse(String),

    /// Provider is unavailable.
    #[error("embedder unavailable: {0}")]
    Unavailable(String),
}

/// Default base URL for the Ollama API.
const DEFAULT_OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434";

/// Ollama-based embedder using the `/api/embed` endpoint.
///
/// Calls `POST {base_url}/api/embed` with the model name and input text,
/// returning the embedding vector.
pub struct OllamaEmbedder {
    model: String,
    client: reqwest::Client,
    base_url: String,
    dims: usize,
}

impl std::fmt::Debug for OllamaEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OllamaEmbedder")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("dims", &self.dims)
            .finish()
    }
}

impl OllamaEmbedder {
    /// Create an Ollama embedder for the given model.
    ///
    /// `dims` is the expected dimensionality of embeddings (e.g. 768 for
    /// nomic-embed-text). This is used by callers to pre-allocate storage.
    pub fn new(model: &str, dims: usize) -> Self {
        Self {
            model: model.to_owned(),
            client: reqwest::Client::new(),
            base_url: DEFAULT_OLLAMA_BASE_URL.to_owned(),
            dims,
        }
    }

    /// Create an Ollama embedder with a custom base URL.
    pub fn with_base_url(model: &str, base_url: &str, dims: usize) -> Self {
        Self {
            model: model.to_owned(),
            client: reqwest::Client::new(),
            base_url: base_url.to_owned(),
            dims,
        }
    }

    /// Build the request body for the embed endpoint.
    fn build_request(&self, text: &str) -> OllamaEmbedRequest {
        OllamaEmbedRequest {
            model: self.model.clone(),
            input: text.to_owned(),
        }
    }
}

#[async_trait]
impl Embedder for OllamaEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedderError> {
        let url = format!("{}/api/embed", self.base_url);
        let body = self.build_request(text);

        let response = self.client.post(&url).json(&body).send().await?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(EmbedderError::Unavailable(format!(
                "ollama returned {status}: {body_text}"
            )));
        }

        let parsed: OllamaEmbedResponse = response
            .json()
            .await
            .map_err(|e| EmbedderError::Parse(e.to_string()))?;

        let embeddings = parsed
            .embeddings
            .into_iter()
            .next()
            .ok_or_else(|| EmbedderError::Parse("empty embeddings array".to_owned()))?;

        Ok(embeddings)
    }

    fn dimensions(&self) -> usize {
        self.dims
    }
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Request body for Ollama `/api/embed`.
#[derive(Debug, Serialize)]
struct OllamaEmbedRequest {
    /// Model name.
    model: String,
    /// Input text to embed.
    input: String,
}

/// Response body from Ollama `/api/embed`.
#[derive(Debug, Deserialize)]
struct OllamaEmbedResponse {
    /// Array of embedding vectors (one per input).
    embeddings: Vec<Vec<f32>>,
}
