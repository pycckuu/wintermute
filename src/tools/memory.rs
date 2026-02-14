//! Memory tool -- save long-term memories to journal (memory spec §4).
//!
//! Provides a single write action:
//! - `memory.save` -- save a fact or preference to long-term memory
//!
//! This is a privileged kernel tool (like `AdminTool`). It holds an
//! `Arc<TaskJournal>` for direct memory writes. It does NOT access
//! the vault or secrets (Invariant B is preserved).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tracing::instrument;

use super::{
    scoped_http::ScopedHttpClient, ActionSemantics, InjectedCredentials, Tool, ToolAction,
    ToolError, ToolManifest, ToolOutput, ValidatedCapability,
};
use crate::kernel::journal::{MemoryRow, TaskJournal};
use crate::types::SecurityLabel;

/// Memory tool for explicit saves (memory spec §4).
///
/// Receives `Arc<TaskJournal>` for direct memory writes. Owner-only —
/// only `principal:owner` can invoke this tool via task templates.
pub struct MemoryTool {
    journal: Arc<TaskJournal>,
}

impl MemoryTool {
    /// Create a new `MemoryTool` with journal access (memory spec §4).
    pub fn new(journal: Arc<TaskJournal>) -> Self {
        Self { journal }
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn manifest(&self) -> ToolManifest {
        ToolManifest {
            name: "memory".to_owned(),
            owner_only: true,
            actions: vec![ToolAction {
                id: "memory.save".to_owned(),
                description: "Save a fact or preference to long-term memory".to_owned(),
                semantics: ActionSemantics::Write,
                label_ceiling: SecurityLabel::Sensitive,
                args_schema: json!({"content": "string"}),
            }],
            network_allowlist: vec![],
        }
    }

    #[instrument(skip(self, _cap, _creds, _http, args), fields(tool = "memory"))]
    async fn execute(
        &self,
        _cap: &ValidatedCapability,
        _creds: &InjectedCredentials,
        _http: &ScopedHttpClient,
        action: &str,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        match action {
            "memory.save" => {
                let content = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidArguments("missing 'content' argument".to_owned())
                    })?;

                let row = MemoryRow {
                    id: uuid::Uuid::new_v4().to_string(),
                    content: content.to_owned(),
                    label: SecurityLabel::Sensitive,
                    source: "explicit".to_owned(),
                    created_at: chrono::Utc::now(),
                    task_id: None,
                };

                self.journal
                    .save_memory(&row)
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

                Ok(ToolOutput {
                    data: json!({"saved": true, "id": row.id}),
                    has_free_text: false,
                })
            }
            other => Err(ToolError::ActionNotFound(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool() -> MemoryTool {
        let journal =
            Arc::new(TaskJournal::open_in_memory().expect("failed to create in-memory journal"));
        MemoryTool::new(journal)
    }

    #[test]
    fn test_manifest_owner_only() {
        let tool = make_tool();
        let manifest = tool.manifest();
        assert_eq!(manifest.name, "memory");
        assert!(manifest.owner_only, "memory tool must be owner-only");
        assert!(
            manifest.network_allowlist.is_empty(),
            "memory tool needs no network"
        );
        assert_eq!(manifest.actions.len(), 1);
        assert_eq!(manifest.actions[0].id, "memory.save");
        assert_eq!(manifest.actions[0].semantics, ActionSemantics::Write);
    }

    #[tokio::test]
    async fn test_save_memory_success() {
        use std::collections::HashSet;
        let tool = make_tool();
        let cap = crate::tools::test_helpers::make_test_capability("memory.save");
        let creds = InjectedCredentials::new();
        let http = ScopedHttpClient::new(HashSet::new());
        let args = json!({"content": "Flight to Bali on March 15th"});

        let output = tool
            .execute(&cap, &creds, &http, "memory.save", args)
            .await
            .expect("save should succeed");

        assert!(!output.has_free_text);
        assert_eq!(output.data["saved"], true);
        assert!(output.data["id"].is_string());

        // Verify entry is in the journal.
        let results = tool
            .journal
            .search_memories("Bali", SecurityLabel::Sensitive, 10)
            .expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "Flight to Bali on March 15th");
    }

    #[tokio::test]
    async fn test_save_missing_content() {
        use std::collections::HashSet;
        let tool = make_tool();
        let cap = crate::tools::test_helpers::make_test_capability("memory.save");
        let creds = InjectedCredentials::new();
        let http = ScopedHttpClient::new(HashSet::new());
        let args = json!({});

        let result = tool.execute(&cap, &creds, &http, "memory.save", args).await;

        assert!(result.is_err());
        let err = result.expect_err("should fail");
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn test_unknown_action() {
        use std::collections::HashSet;
        let tool = make_tool();
        let cap = crate::tools::test_helpers::make_test_capability("memory.delete");
        let creds = InjectedCredentials::new();
        let http = ScopedHttpClient::new(HashSet::new());
        let args = json!({});

        let result = tool
            .execute(&cap, &creds, &http, "memory.delete", args)
            .await;

        assert!(result.is_err());
        let err = result.expect_err("should fail");
        assert!(matches!(err, ToolError::ActionNotFound(_)));
    }
}
