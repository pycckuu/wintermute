//! Tests for `src/memory/embedder.rs` — Embedder trait and OllamaEmbedder.

use wintermute::memory::embedder::{Embedder, OllamaEmbedder};

#[test]
fn ollama_embedder_reports_correct_dimensions() {
    let embedder = OllamaEmbedder::new("nomic-embed-text", 768);
    assert_eq!(embedder.dimensions(), 768);
}

#[test]
fn ollama_embedder_custom_base_url_reports_dimensions() {
    let embedder = OllamaEmbedder::with_base_url("test-model", "http://custom:1234", 384);
    assert_eq!(embedder.dimensions(), 384);
}

#[test]
fn ollama_embedder_debug_includes_model_name() {
    let embedder = OllamaEmbedder::new("nomic-embed-text", 768);
    let debug = format!("{embedder:?}");
    assert!(debug.contains("nomic-embed-text"));
    assert!(debug.contains("OllamaEmbedder"));
}

#[test]
fn ollama_embedder_debug_includes_base_url() {
    let embedder = OllamaEmbedder::with_base_url("model", "http://custom:9999", 512);
    let debug = format!("{embedder:?}");
    assert!(debug.contains("http://custom:9999"));
}

#[tokio::test]
async fn ollama_embedder_returns_error_when_unavailable() {
    // Point at a port nothing listens on.
    let embedder = OllamaEmbedder::with_base_url("test-model", "http://127.0.0.1:1", 768);
    let result = embedder.embed("test text").await;
    assert!(result.is_err(), "should fail when Ollama is unreachable");
}
