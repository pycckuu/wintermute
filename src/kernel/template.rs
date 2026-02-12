/// Task template engine — loading, matching, and validation (spec 4.5, 18.2).
///
/// Every task is instantiated from a `TaskTemplate` that defines its capability
/// ceiling. The Planner can only select tools within the template's bounds.
use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::types::{PrincipalClass, SecurityLabel};

/// Error type for template operations.
#[derive(Debug, Error)]
pub enum TemplateError {
    /// Template with given ID not found in registry.
    #[error("template not found: {0}")]
    NotFound(String),
    /// Failed to parse a template TOML file.
    #[error("failed to parse template TOML: {0}")]
    ParseError(#[from] toml::de::Error),
    /// I/O error reading template files.
    #[error("failed to read template file: {0}")]
    IoError(#[from] std::io::Error),
    /// No template matched the given trigger and principal class.
    #[error("no template matches trigger '{trigger}' for principal class {class:?}")]
    NoMatch {
        trigger: String,
        class: PrincipalClass,
    },
}

/// Inference routing configuration within a template (spec 11.1).
#[derive(Debug, Clone, Deserialize)]
pub struct InferenceConfig {
    /// LLM provider: "local", "anthropic", "openai".
    pub provider: String,
    /// Model identifier (e.g. "llama3", "claude-sonnet-4-20250514").
    pub model: String,
    /// Owner has acknowledged cloud risk for sensitive data.
    #[serde(default)]
    pub owner_acknowledged_cloud_risk: bool,
}

/// Task template loaded from TOML (spec 4.5, 18.2).
///
/// Defines the capability ceiling for tasks triggered by matching events.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskTemplate {
    /// Unique identifier for this template.
    pub template_id: String,
    /// Trigger patterns that activate this template (e.g. "adapter:telegram:message:owner").
    pub triggers: Vec<String>,
    /// Required principal class for this template.
    pub principal_class: PrincipalClass,
    /// Human-readable description of the template's purpose.
    pub description: String,
    /// Static task description shown to Planner for third-party triggers (spec 7).
    #[serde(default)]
    pub planner_task_description: Option<String>,
    /// Tools this template allows.
    pub allowed_tools: Vec<String>,
    /// Tools explicitly denied (overrides allowed).
    #[serde(default)]
    pub denied_tools: Vec<String>,
    /// Maximum number of tool calls per task.
    pub max_tool_calls: u32,
    /// Token budget for the Planner phase.
    #[serde(default = "default_max_tokens_plan")]
    pub max_tokens_plan: u32,
    /// Token budget for the Synthesizer phase.
    #[serde(default = "default_max_tokens_synthesize")]
    pub max_tokens_synthesize: u32,
    /// Output sinks for this template.
    pub output_sinks: Vec<String>,
    /// Maximum security label for data this template may handle.
    pub data_ceiling: SecurityLabel,
    /// Inference routing configuration.
    pub inference: InferenceConfig,
    /// Whether write operations always require human approval.
    #[serde(default)]
    pub require_approval_for_writes: bool,
}

fn default_max_tokens_plan() -> u32 {
    4000
}
fn default_max_tokens_synthesize() -> u32 {
    8000
}

/// Registry of task templates indexed for fast matching (spec 4.5).
pub struct TemplateRegistry {
    templates: HashMap<String, TaskTemplate>,
}

impl TemplateRegistry {
    /// Create an empty template registry.
    pub fn new() -> Self {
        Self {
            templates: HashMap::new(),
        }
    }

    /// Register a single template.
    pub fn register(&mut self, template: TaskTemplate) {
        self.templates
            .insert(template.template_id.clone(), template);
    }

    /// Load all `.toml` files from a directory as templates.
    pub fn load_from_dir(path: impl AsRef<Path>) -> Result<Self, TemplateError> {
        let mut registry = Self::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let file_path = entry.path();
            if file_path.extension().and_then(|e| e.to_str()) == Some("toml") {
                let contents = std::fs::read_to_string(&file_path)?;
                let template: TaskTemplate = toml::from_str(&contents)?;
                registry.register(template);
            }
        }
        Ok(registry)
    }

    /// Find the first template matching a trigger string and principal class.
    pub fn match_template(
        &self,
        trigger: &str,
        principal_class: PrincipalClass,
    ) -> Option<&TaskTemplate> {
        self.templates.values().find(|t| {
            t.principal_class == principal_class && t.triggers.iter().any(|tr| tr == trigger)
        })
    }

    /// Get a template by its ID.
    pub fn get(&self, id: &str) -> Option<&TaskTemplate> {
        self.templates.get(id)
    }
}

impl Default for TemplateRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_TEMPLATE_TOML: &str = r#"
template_id = "owner_telegram_general"
triggers = ["adapter:telegram:message:owner"]
principal_class = "owner"
description = "General assistant for owner via Telegram"
allowed_tools = ["email.list", "email.read", "calendar.freebusy"]
denied_tools = []
max_tool_calls = 15
output_sinks = ["sink:telegram:owner"]
data_ceiling = "sensitive"

[inference]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
owner_acknowledged_cloud_risk = true
"#;

    const THIRD_PARTY_TEMPLATE_TOML: &str = r#"
template_id = "whatsapp_scheduling"
triggers = ["adapter:whatsapp:message:third_party"]
principal_class = "third_party"
description = "Handle scheduling requests"
planner_task_description = "A contact is requesting to schedule a meeting."
allowed_tools = ["calendar.freebusy", "message.reply"]
denied_tools = ["email.send"]
max_tool_calls = 5
output_sinks = ["sink:whatsapp:reply_to_sender"]
data_ceiling = "internal"

[inference]
provider = "local"
model = "llama3"
"#;

    #[test]
    fn test_parse_valid_template() {
        let t: TaskTemplate = toml::from_str(VALID_TEMPLATE_TOML).expect("should parse");
        assert_eq!(t.template_id, "owner_telegram_general");
        assert_eq!(t.principal_class, PrincipalClass::Owner);
        assert_eq!(t.data_ceiling, SecurityLabel::Sensitive);
        assert_eq!(t.max_tool_calls, 15);
        assert!(t.inference.owner_acknowledged_cloud_risk);
        assert_eq!(t.max_tokens_plan, 4000); // default
    }

    #[test]
    fn test_parse_third_party_template() {
        let t: TaskTemplate = toml::from_str(THIRD_PARTY_TEMPLATE_TOML).expect("should parse");
        assert_eq!(t.principal_class, PrincipalClass::ThirdParty);
        assert_eq!(t.data_ceiling, SecurityLabel::Internal);
        assert_eq!(
            t.planner_task_description.as_deref(),
            Some("A contact is requesting to schedule a meeting.")
        );
        assert!(!t.inference.owner_acknowledged_cloud_risk);
    }

    #[test]
    fn test_match_template_found() {
        let mut reg = TemplateRegistry::new();
        reg.register(toml::from_str(VALID_TEMPLATE_TOML).expect("parse"));

        let matched = reg.match_template("adapter:telegram:message:owner", PrincipalClass::Owner);
        assert!(matched.is_some());
        assert_eq!(
            matched.expect("matched").template_id,
            "owner_telegram_general"
        );
    }

    #[test]
    fn test_match_template_wrong_class() {
        let mut reg = TemplateRegistry::new();
        reg.register(toml::from_str(VALID_TEMPLATE_TOML).expect("parse"));

        let matched =
            reg.match_template("adapter:telegram:message:owner", PrincipalClass::ThirdParty);
        assert!(matched.is_none());
    }

    #[test]
    fn test_match_template_not_found() {
        let mut reg = TemplateRegistry::new();
        reg.register(toml::from_str(VALID_TEMPLATE_TOML).expect("parse"));

        let matched = reg.match_template("adapter:slack:message:owner", PrincipalClass::Owner);
        assert!(matched.is_none());
    }

    #[test]
    fn test_load_from_dir() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        std::fs::write(dir.path().join("owner.toml"), VALID_TEMPLATE_TOML).expect("write template");
        std::fs::write(
            dir.path().join("scheduling.toml"),
            THIRD_PARTY_TEMPLATE_TOML,
        )
        .expect("write template");

        let reg = TemplateRegistry::load_from_dir(dir.path()).expect("load");
        assert!(reg.get("owner_telegram_general").is_some());
        assert!(reg.get("whatsapp_scheduling").is_some());
    }

    #[test]
    fn test_get_by_id() {
        let mut reg = TemplateRegistry::new();
        reg.register(toml::from_str(VALID_TEMPLATE_TOML).expect("parse"));

        assert!(reg.get("owner_telegram_general").is_some());
        assert!(reg.get("nonexistent").is_none());
    }
}
