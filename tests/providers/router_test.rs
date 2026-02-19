//! Integration tests for model router precedence behavior.

use std::collections::{BTreeMap, HashMap};

use wintermute::config::ModelsConfig;
use wintermute::credentials::Credentials;
use wintermute::providers::router::ModelRouter;

#[test]
fn resolves_skill_before_role_before_default() {
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
    let router_result = ModelRouter::from_config(&models, &credentials);
    assert!(router_result.is_ok());
    let router = match router_result {
        Ok(router) => router,
        Err(err) => panic!("router should initialize: {err}"),
    };

    let resolved = router.resolve_spec(Some("observer"), Some("deploy_check"));
    assert_eq!(resolved, "anthropic/claude-haiku-4-5-20251001");
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
    let router_result = ModelRouter::from_config(&models, &credentials);
    assert!(router_result.is_ok());
    let router = match router_result {
        Ok(router) => router,
        Err(err) => panic!("router should initialize with default provider: {err}"),
    };

    let resolved = router.resolve_spec(None, Some("deploy_check"));
    assert_eq!(resolved, "ollama/qwen3:8b");
}
