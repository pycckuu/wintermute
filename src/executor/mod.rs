//! Command execution abstractions and implementations.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;

pub mod direct;
pub mod docker;
pub mod redactor;

/// Executor implementation kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorKind {
    /// Docker-backed sandbox executor.
    Docker,
    /// Host-local maintenance executor.
    Direct,
}

/// Command execution options.
#[derive(Debug, Clone)]
pub struct ExecOptions {
    /// Maximum command runtime before timeout handling.
    pub timeout: Duration,
    /// Optional working directory inside executor context.
    pub working_dir: Option<PathBuf>,
}

impl Default for ExecOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
            working_dir: None,
        }
    }
}

/// Command execution result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    /// Process exit code (`None` when the process was killed or exit code unavailable).
    pub exit_code: Option<i32>,
    /// Captured stdout text.
    pub stdout: String,
    /// Captured stderr text.
    pub stderr: String,
    /// Whether the command exceeded the timeout.
    pub timed_out: bool,
    /// Wall-clock duration of the execution.
    pub duration: Duration,
}

impl ExecResult {
    /// Returns `true` when the command exited successfully (code 0, no timeout).
    pub fn success(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }

    /// Combined stdout+stderr output, separated by a newline when both are non-empty.
    pub fn output(&self) -> String {
        if self.stdout.is_empty() {
            return self.stderr.clone();
        }
        if self.stderr.is_empty() {
            return self.stdout.clone();
        }
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

/// Health status for a concrete executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    /// Executor is operational.
    Healthy {
        /// Executor implementation kind.
        kind: ExecutorKind,
        /// Human-readable diagnostics.
        details: String,
    },
    /// Executor exists but is in a degraded state.
    Degraded {
        /// Executor implementation kind.
        kind: ExecutorKind,
        /// Human-readable diagnostics.
        details: String,
    },
    /// Executor is not available.
    Unavailable {
        /// Executor implementation kind.
        kind: ExecutorKind,
        /// Human-readable diagnostics.
        details: String,
    },
}

impl HealthStatus {
    /// Returns `true` when the executor is in a healthy state.
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy { .. })
    }
}

/// Errors produced by executor operations.
#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    /// Infrastructure client failure.
    #[error("executor operation failed: {0}")]
    Infrastructure(String),
    /// Command execution exceeded timeout.
    #[error("command timed out after {seconds}s")]
    Timeout {
        /// Timeout budget in seconds.
        seconds: u64,
    },
    /// Command execution is not permitted in this mode.
    #[error("execution is not allowed in this mode: {0}")]
    Forbidden(String),
}

/// Unified executor trait used by runtime command execution.
#[async_trait]
pub trait Executor: Send + Sync {
    /// Execute a command with options and capture output.
    async fn execute(&self, command: &str, opts: ExecOptions) -> Result<ExecResult, ExecutorError>;
    /// Check health for this executor instance.
    async fn health_check(&self) -> Result<HealthStatus, ExecutorError>;
    /// Whether this executor provides network isolation.
    fn has_network_isolation(&self) -> bool;
    /// Returns scripts directory for dynamic tools.
    fn scripts_dir(&self) -> &Path;
    /// Returns workspace directory for command execution.
    fn workspace_dir(&self) -> &Path;
    /// Returns concrete executor kind.
    fn kind(&self) -> ExecutorKind;
}

/// Detect the available executor kind at runtime.
///
/// Returns [`ExecutorKind::Docker`] when the Docker daemon is reachable,
/// otherwise falls back to [`ExecutorKind::Direct`].
pub async fn auto_detect() -> ExecutorKind {
    if docker::DockerExecutor::docker_available().await {
        ExecutorKind::Docker
    } else {
        ExecutorKind::Direct
    }
}
