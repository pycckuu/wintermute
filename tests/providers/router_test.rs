//! Integration tests for model router precedence behavior.

use std::collections::{BTreeMap, HashMap};

use wintermute::config::ModelsConfig;
use wintermute::credentials::Credentials;
use wintermute::providers::router::ModelRouter;

fn ollama_default_config() -> ModelsConfig {
    ModelsConfig {
        default: "ollama/qwen3:8b".to_owned(),
        roles: HashMap::new(),
        skills: HashMap::new(),
    }
}

fn openai_default_config() -> ModelsConfig {
    ModelsConfig {
        default: "openai/gpt-5".to_owned(),
        roles: HashMap::new(),
        skills: HashMap::new(),
    }
}

fn multi_provider_config() -> (ModelsConfig, Credentials) {
    let models = ModelsConfig {
        default: "ollama/qwen3:8b".to_owned(),
        roles: HashMap::from([("observer".to_owned(), "ollama/qwen3:8b".to_owned())]),
        skills: HashMap::from([(
            "deploy_check".to_owned(),
            "anthropic/claude-haiku-4-5-20251001".to_owned(),
        )]),
    };
    let mut vars = BTreeMap::new();
    vars.insert("ANTHROPIC_API_KEY".to_owned(), "test-key".to_owned());
    let credentials = Credentials::from_map(vars);
    (models, credentials)
}

fn openai_credentials_with_oauth_and_api_key() -> Credentials {
    let mut vars = BTreeMap::new();
    vars.insert("OPENAI_OAUTH_TOKEN".to_owned(), "oauth-token".to_owned());
    vars.insert("OPENAI_API_KEY".to_owned(), "api-key".to_owned());
    Credentials::from_map(vars)
}

#[test]
fn resolves_skill_before_role_before_default() {
    let (models, credentials) = multi_provider_config();
    let router = ModelRouter::from_config(&models, &credentials).expect("router should init");
    let resolved = router.resolve_spec(Some("observer"), Some("deploy_check"));
    assert_eq!(resolved, "anthropic/claude-haiku-4-5-20251001");
}

#[test]
fn falls_back_to_role_when_no_skill() {
    let (models, credentials) = multi_provider_config();
    let router = ModelRouter::from_config(&models, &credentials).expect("router should init");
    let resolved = router.resolve_spec(Some("observer"), None);
    assert_eq!(resolved, "ollama/qwen3:8b");
}

#[test]
fn falls_back_to_default_when_skill_provider_unavailable() {
    let models = ModelsConfig {
        default: "ollama/qwen3:8b".to_owned(),
        roles: HashMap::new(),
        skills: HashMap::from([(
            "deploy_check".to_owned(),
            "anthropic/claude-haiku-4-5-20251001".to_owned(),
        )]),
    };
    let credentials = Credentials::default();
    let router = ModelRouter::from_config(&models, &credentials).expect("router should init");
    let resolved = router.resolve_spec(None, Some("deploy_check"));
    assert_eq!(resolved, "ollama/qwen3:8b");
}

#[test]
fn has_model_returns_true_for_registered() {
    let models = ollama_default_config();
    let credentials = Credentials::default();
    let router = ModelRouter::from_config(&models, &credentials).expect("router should init");
    assert!(router.has_model("ollama/qwen3:8b"));
    assert!(!router.has_model("anthropic/nonexistent"));
}

#[test]
fn available_specs_returns_sorted() {
    let (models, credentials) = multi_provider_config();
    let router = ModelRouter::from_config(&models, &credentials).expect("router should init");
    let specs = router.available_specs();
    assert!(specs.len() >= 2);
    let sorted: Vec<String> = {
        let mut s = specs.clone();
        s.sort();
        s
    };
    assert_eq!(specs, sorted);
}

#[test]
fn default_provider_returns_configured_default() {
    let models = ollama_default_config();
    let credentials = Credentials::default();
    let router = ModelRouter::from_config(&models, &credentials).expect("router should init");
    let provider = router.default_provider();
    assert_eq!(provider.model_id(), "ollama/qwen3:8b");
}

#[test]
fn provider_count_matches_loaded() {
    let (models, credentials) = multi_provider_config();
    let router = ModelRouter::from_config(&models, &credentials).expect("router should init");
    assert!(router.provider_count() >= 2);
}

#[test]
fn router_errors_on_unavailable_default() {
    let models = ModelsConfig {
        default: "anthropic/claude-sonnet".to_owned(),
        roles: HashMap::new(),
        skills: HashMap::new(),
    };
    let credentials = Credentials::default(); // no API key
    let result = ModelRouter::from_config(&models, &credentials);
    assert!(result.is_err());
}

#[test]
fn resolve_returns_provider_for_valid_spec() {
    let models = ollama_default_config();
    let credentials = Credentials::default();
    let router = ModelRouter::from_config(&models, &credentials).expect("router should init");
    let provider = router.resolve(None, None);
    assert!(provider.is_ok());
    assert_eq!(
        provider.expect("should resolve").model_id(),
        "ollama/qwen3:8b"
    );
}

#[test]
fn router_loads_openai_default_with_oauth_token() {
    let models = openai_default_config();
    let credentials = openai_credentials_with_oauth_and_api_key();
    let router = ModelRouter::from_config(&models, &credentials).expect("router should init");
    assert!(router.has_model("openai/gpt-5"));
    assert_eq!(router.default_provider().model_id(), "openai/gpt-5");
}

#[test]
fn router_loads_openai_default_with_api_key_fallback() {
    let models = openai_default_config();
    let mut vars = BTreeMap::new();
    vars.insert("OPENAI_API_KEY".to_owned(), "api-key".to_owned());
    let credentials = Credentials::from_map(vars);
    let router = ModelRouter::from_config(&models, &credentials).expect("router should init");
    assert!(router.has_model("openai/gpt-5"));
}

#[test]
fn router_errors_on_unavailable_openai_default() {
    let models = openai_default_config();
    let credentials = Credentials::default();
    let result = ModelRouter::from_config(&models, &credentials);
    assert!(result.is_err());
}

#[test]
fn resolves_openai_skill_override_when_credentials_present() {
    let models = ModelsConfig {
        default: "ollama/qwen3:8b".to_owned(),
        roles: HashMap::new(),
        skills: HashMap::from([("gpt_task".to_owned(), "openai/gpt-5".to_owned())]),
    };
    let mut vars = BTreeMap::new();
    vars.insert("OPENAI_API_KEY".to_owned(), "api-key".to_owned());
    let credentials = Credentials::from_map(vars);
    let router = ModelRouter::from_config(&models, &credentials).expect("router should init");
    assert_eq!(router.resolve_spec(None, Some("gpt_task")), "openai/gpt-5");
}
