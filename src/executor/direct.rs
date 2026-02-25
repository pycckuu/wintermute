//! Direct executor used only for local maintenance checks.

use std::path::{Path, PathBuf};

use super::{ExecOptions, ExecResult, Executor, ExecutorError, ExecutorKind, HealthStatus};

/// Direct host executor in maintenance-only mode.
#[derive(Debug, Clone)]
pub struct DirectExecutor {
    scripts_dir: PathBuf,
    workspace_dir: PathBuf,
}

impl DirectExecutor {
    /// Create a maintenance-only direct executor.
    pub fn new(scripts_dir: PathBuf, workspace_dir: PathBuf) -> Self {
        Self {
            scripts_dir,
            workspace_dir,
        }
    }
}

/// Resolve a working directory relative to a base, with path traversal protection.
///
/// Returns `Err` if the resolved path escapes the base directory.
///
/// # Errors
///
/// Returns `ExecutorError::Forbidden` when the requested path would escape the base.
#[doc(hidden)]
pub fn resolve_working_dir(base: &Path, requested: &Path) -> Result<PathBuf, ExecutorError> {
    let resolved = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        base.join(requested)
    };

    // Canonicalize would require the path to exist. Instead, check
    // that the normalized path starts with the base.
    let normalized = normalize_path(&resolved);
    let base_normalized = normalize_path(base);

    if !normalized.starts_with(&base_normalized) {
        return Err(ExecutorError::Forbidden(format!(
            "working directory '{}' escapes base '{}'",
            requested.display(),
            base.display()
        )));
    }

    Ok(normalized)
}

/// Normalize a path by resolving `.` and `..` components without filesystem access.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

#[async_trait::async_trait]
impl Executor for DirectExecutor {
    async fn execute(
        &self,
        _command: &str,
        _opts: ExecOptions,
    ) -> Result<ExecResult, ExecutorError> {
        Err(ExecutorError::Forbidden(
            "direct executor is maintenance-only and disabled for agent command execution"
                .to_owned(),
        ))
    }

    async fn health_check(&self) -> Result<HealthStatus, ExecutorError> {
        Ok(HealthStatus::Degraded {
            kind: ExecutorKind::Direct,
            details: "direct executor available (maintenance-only mode, no sandbox)".to_owned(),
        })
    }

    fn scripts_dir(&self) -> &Path {
        &self.scripts_dir
    }

    fn workspace_dir(&self) -> &Path {
        &self.workspace_dir
    }

    fn kind(&self) -> ExecutorKind {
        ExecutorKind::Direct
    }
}
