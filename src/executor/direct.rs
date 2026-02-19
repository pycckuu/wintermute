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
        Ok(HealthStatus {
            is_healthy: true,
            kind: ExecutorKind::Direct,
            details: "direct executor available (maintenance-only mode)".to_owned(),
        })
    }

    fn has_network_isolation(&self) -> bool {
        false
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
