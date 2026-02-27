//! Tool creation: writes implementation + schema to /scripts/ and commits to git.
//!
//! The [`create_tool`] function validates the tool name, writes the Python
//! implementation and JSON schema to the scripts directory, commits the
//! changes to git, and reloads the registry.

use serde_json::json;
use tracing::debug;

use crate::executor::docker::shell_escape;
use crate::executor::{ExecOptions, Executor};

use super::registry::{DynamicToolRegistry, ToolMeta};
use super::ToolError;

/// Maximum allowed tool name length.
const MAX_TOOL_NAME_LEN: usize = 64;

/// Default timeout for executor commands during tool creation.
const CREATE_TOOL_TIMEOUT_SECS: u64 = 30;

/// Build the standard exec options for tool creation commands.
fn create_tool_exec_opts() -> ExecOptions {
    ExecOptions {
        timeout: std::time::Duration::from_secs(CREATE_TOOL_TIMEOUT_SECS),
        working_dir: None,
    }
}

/// Validate a dynamic tool name.
///
/// Rules:
/// - Only `[a-z0-9_]` characters allowed
/// - Must start with a lowercase letter
/// - Maximum 64 characters
/// - Cannot start with `_system`
/// - Cannot contain `..`, `/`, or `\`
///
/// # Errors
///
/// Returns [`ToolError::InvalidInput`] if the name violates any rule.
pub fn validate_tool_name(name: &str) -> Result<(), ToolError> {
    if name.is_empty() {
        return Err(ToolError::InvalidInput(
            "tool name cannot be empty".to_owned(),
        ));
    }

    if name.len() > MAX_TOOL_NAME_LEN {
        return Err(ToolError::InvalidInput(format!(
            "tool name exceeds {MAX_TOOL_NAME_LEN} characters"
        )));
    }

    if name.starts_with("_system") {
        return Err(ToolError::InvalidInput(
            "tool name cannot start with _system".to_owned(),
        ));
    }

    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err(ToolError::InvalidInput(
            "tool name cannot contain '..', '/', or '\\'".to_owned(),
        ));
    }

    // Must start with a lowercase letter.
    let first = name
        .chars()
        .next()
        .ok_or_else(|| ToolError::InvalidInput("tool name cannot be empty".to_owned()))?;
    if !first.is_ascii_lowercase() {
        return Err(ToolError::InvalidInput(
            "tool name must start with a lowercase letter".to_owned(),
        ));
    }

    // Only [a-z0-9_] allowed.
    for ch in name.chars() {
        if !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && ch != '_' {
            return Err(ToolError::InvalidInput(format!(
                "tool name contains invalid character: '{ch}'"
            )));
        }
    }

    Ok(())
}

/// Create or update a dynamic tool.
///
/// Writes the Python implementation and JSON schema to `/scripts/`, commits
/// the changes to git, and reloads the tool in the registry.
///
/// # Errors
///
/// Returns [`ToolError`] on validation failure, write failure, or git failure.
pub async fn create_tool(
    executor: &dyn Executor,
    registry: &DynamicToolRegistry,
    name: &str,
    description: &str,
    parameters_schema: &serde_json::Value,
    implementation: &str,
    timeout_secs: u64,
) -> Result<String, ToolError> {
    validate_tool_name(name)?;

    let scripts_dir = executor.scripts_dir().display().to_string();

    // Check if files already exist (for commit message).
    let check_cmd = format!("test -f {scripts_dir}/{name}.py && echo update || echo create");
    let check_result = executor
        .execute(&check_cmd, create_tool_exec_opts())
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to check existing tool: {e}")))?;

    let action = if check_result.stdout.trim() == "update" {
        "update"
    } else {
        "create"
    };

    // Step 1: Write implementation file.
    let escaped_impl = shell_escape(implementation);
    let write_impl_cmd = format!(
        "printf '%s' {escaped_impl} > {scripts_dir}/{name}.py && chmod +x {scripts_dir}/{name}.py"
    );
    executor
        .execute(&write_impl_cmd, create_tool_exec_opts())
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to write implementation: {e}")))?;

    debug!(tool = name, "wrote implementation file");

    // Step 2: Write schema JSON file with _meta.
    let meta = if action == "update" {
        // Preserve existing _meta, bump version.
        registry
            .get(name)
            .and_then(|s| s.meta)
            .map(|mut m| {
                m.version = m.version.saturating_add(1);
                m
            })
            .unwrap_or_else(ToolMeta::new_initial)
    } else {
        ToolMeta::new_initial()
    };

    let meta_json = serde_json::to_value(&meta)
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to serialize _meta: {e}")))?;

    let schema = json!({
        "name": name,
        "description": description,
        "parameters": parameters_schema,
        "timeout_secs": timeout_secs,
        "_meta": meta_json,
    });
    let schema_json = serde_json::to_string_pretty(&schema)
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to serialize schema: {e}")))?;
    let escaped_schema = shell_escape(&schema_json);
    let write_schema_cmd = format!("printf '%s' {escaped_schema} > {scripts_dir}/{name}.json");
    executor
        .execute(&write_schema_cmd, create_tool_exec_opts())
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to write schema: {e}")))?;

    debug!(tool = name, "wrote schema file");

    // Step 3: Git commit.
    let escaped_name = shell_escape(name);
    let git_cmd = format!(
        "cd {scripts_dir} && git add {escaped_name}.py {escaped_name}.json && git commit -m '{action} tool: {name}'"
    );
    let git_result = executor.execute(&git_cmd, create_tool_exec_opts()).await;

    // Git commit may fail if git is not configured — log but don't fail.
    match git_result {
        Ok(result) if result.success() => {
            debug!(tool = name, "git commit succeeded");
        }
        Ok(result) => {
            tracing::warn!(
                tool = name,
                stderr = %result.stderr,
                "git commit returned non-zero but tool was created"
            );
        }
        Err(e) => {
            tracing::warn!(
                tool = name,
                error = %e,
                "git commit failed but tool was created"
            );
        }
    }

    // Step 4: Reload registry.
    if let Err(e) = registry.reload_tool(name) {
        tracing::warn!(tool = name, error = %e, "failed to reload tool in registry");
    }

    Ok(format!("Tool '{name}' {action}d successfully"))
}
