//! Integration tests for `src/providers/`.

#[path = "providers/anthropic_test.rs"]
mod anthropic_test;
#[path = "providers/http_response_test.rs"]
mod http_response_test;
#[path = "providers/ollama_test.rs"]
mod ollama_test;
#[path = "providers/openai_test.rs"]
mod openai_test;
#[path = "providers/provider_contract_test.rs"]
mod provider_contract_test;
#[path = "providers/router_test.rs"]
mod router_test;
#[path = "providers/types_test.rs"]
mod types_test;
