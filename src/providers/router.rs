//! Model router resolving providers by skill, role, and default settings.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;

use crate::config::{all_model_specs, ModelsConfig};
use crate::credentials::{resolve_anthropic_auth, Credentials};

use super::anthropic::AnthropicProvider;
use super::ollama::OllamaProvider;
use super::LlmProvider;

/// Provider routing errors.
#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    /// Model spec is not in `<provider>/<model>` format.
    #[error("invalid model spec '{spec}', expected '<provider>/<model>'")]
    InvalidModelSpec {
        /// Invalid raw spec.
        spec: String,
    },
    /// The requested provider spec is not available.
    #[error("provider not available for model spec '{spec}'")]
    UnavailableProvider {
        /// Unavailable spec.
        spec: String,
    },
    /// Default provider spec could not be created.
    #[error("default provider '{spec}' is unavailable")]
    DefaultUnavailable {
        /// Missing default spec.
        spec: String,
    },
    /// Unsupported provider type in spec prefix.
    #[error("unsupported provider '{provider}'")]
    UnsupportedProvider {
        /// Unsupported provider prefix.
        provider: String,
    },
    /// Required API credential missing for selected provider.
    #[error("missing credential for provider '{provider}': {key}")]
    MissingCredential {
        /// Provider name.
        provider: String,
        /// Missing credential key.
        key: String,
    },
}

/// Model router resolving `skill -> role -> default`.
#[derive(Clone)]
pub struct ModelRouter {
    providers: HashMap<String, Arc<dyn LlmProvider>>,
    default: String,
    role_overrides: HashMap<String, String>,
    skill_overrides: HashMap<String, String>,
}

impl ModelRouter {
    /// Build a router from model config and loaded credentials.
    ///
    /// # Errors
    ///
    /// Returns an error if the default provider cannot be instantiated.
    pub fn from_config(models: &ModelsConfig, credentials: &Credentials) -> anyhow::Result<Self> {
        let mut providers: HashMap<String, Arc<dyn LlmProvider>> = HashMap::new();
        let specs = all_model_specs(models);

        for spec in specs {
            let parsed = parse_model_spec(&spec)
                .with_context(|| format!("failed to parse model spec '{spec}'"))?;
            let instance =
                instantiate_provider(&spec, &parsed.provider, &parsed.model, credentials);
            if let Ok(provider) = instance {
                providers.insert(spec.clone(), provider);
            }
        }

        if !providers.contains_key(&models.default) {
            return Err(RouterError::DefaultUnavailable {
                spec: models.default.clone(),
            }
            .into());
        }

        Ok(Self {
            providers,
            default: models.default.clone(),
            role_overrides: models.roles.clone(),
            skill_overrides: models.skills.clone(),
        })
    }

    /// Create a router backed by a single provider for integration tests.
    #[doc(hidden)]
    pub fn for_testing(default_spec: String, provider: Arc<dyn LlmProvider>) -> Self {
        let mut providers = HashMap::new();
        providers.insert(default_spec.clone(), provider);
        Self {
            providers,
            default: default_spec,
            role_overrides: HashMap::new(),
            skill_overrides: HashMap::new(),
        }
    }

    /// Resolve a provider by optional role and skill identifiers.
    ///
    /// Resolution order: `skill -> role -> default`.
    ///
    /// # Errors
    ///
    /// Returns an error if no provider can be resolved.
    pub fn resolve(
        &self,
        role: Option<&str>,
        skill: Option<&str>,
    ) -> anyhow::Result<Arc<dyn LlmProvider>> {
        let selected = self.resolve_spec(role, skill);
        self.providers
            .get(&selected)
            .cloned()
            .ok_or_else(|| RouterError::UnavailableProvider { spec: selected }.into())
    }

    /// Resolve a model spec string by optional role and skill.
    pub fn resolve_spec(&self, role: Option<&str>, skill: Option<&str>) -> String {
        if let Some(spec) = skill
            .and_then(|s| self.skill_overrides.get(s))
            .filter(|spec| self.providers.contains_key(*spec))
        {
            return spec.clone();
        }
        if let Some(spec) = role
            .and_then(|r| self.role_overrides.get(r))
            .filter(|spec| self.providers.contains_key(*spec))
        {
            return spec.clone();
        }
        self.default.clone()
    }

    /// Returns true when a specific model spec is available.
    pub fn has_model(&self, spec: &str) -> bool {
        self.providers.contains_key(spec)
    }

    /// Returns the default provider.
    pub fn default_provider(&self) -> Arc<dyn LlmProvider> {
        // Safe: from_config guarantees the default is present.
        Arc::clone(&self.providers[&self.default])
    }

    /// Returns the number of loaded providers.
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    /// Returns all available provider specs in sorted order.
    pub fn available_specs(&self) -> Vec<String> {
        let mut values: Vec<String> = self.providers.keys().cloned().collect();
        values.sort();
        values
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedModelSpec {
    provider: String,
    model: String,
}

fn parse_model_spec(spec: &str) -> Result<ParsedModelSpec, RouterError> {
    let mut split = spec.splitn(2, '/');
    let provider = split.next().unwrap_or_default();
    let model = split.next().unwrap_or_default();
    if provider.is_empty() || model.is_empty() {
        return Err(RouterError::InvalidModelSpec {
            spec: spec.to_owned(),
        });
    }
    Ok(ParsedModelSpec {
        provider: provider.to_owned(),
        model: model.to_owned(),
    })
}

fn instantiate_provider(
    model_spec: &str,
    provider: &str,
    model: &str,
    credentials: &Credentials,
) -> Result<Arc<dyn LlmProvider>, RouterError> {
    match provider {
        "anthropic" => {
            let auth = resolve_anthropic_auth(credentials).ok_or_else(|| {
                RouterError::MissingCredential {
                    provider: provider.to_owned(),
                    key: "ANTHROPIC_API_KEY or OAuth token".to_owned(),
                }
            })?;
            Ok(Arc::new(AnthropicProvider::new(
                model_spec.to_owned(),
                model.to_owned(),
                auth,
            )))
        }
        "ollama" => Ok(Arc::new(OllamaProvider::new(
            model_spec.to_owned(),
            model.to_owned(),
        ))),
        _ => Err(RouterError::UnsupportedProvider {
            provider: provider.to_owned(),
        }),
    }
}
