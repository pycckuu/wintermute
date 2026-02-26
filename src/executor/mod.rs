//! Command execution abstractions and implementations.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use bollard::image::{BuildImageOptions, CreateImageOptions};
use bollard::models::BuildInfo;
use bollard::Docker;
use bytes::Bytes;
use tokio_stream::StreamExt;

pub mod direct;
pub mod docker;
pub mod egress;
pub mod playwright;
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

/// Maximum time allowed for pulling or building a Docker image before giving up.
const IMAGE_TIMEOUT: Duration = Duration::from_secs(300);

/// Ensure a Docker image is available locally, pulling it if necessary.
///
/// Checks local availability via `inspect_image` before attempting a pull,
/// avoiding unnecessary network round-trips when the image is already present.
/// The pull is bounded by a 300-second timeout to prevent indefinite hangs.
///
/// When `dockerfile` is provided, a failed pull falls back to building the
/// image locally from the embedded Dockerfile content. This keeps the
/// development workflow functional without requiring registry authentication
/// or a published release.
///
/// Returns [`ExecutorError::Infrastructure`] if both pull and build fail.
pub async fn ensure_image(
    docker: &Docker,
    image: &str,
    dockerfile: Option<&str>,
) -> Result<(), ExecutorError> {
    // Fast path: image already present locally.
    if docker.inspect_image(image).await.is_ok() {
        tracing::debug!(%image, "image already available locally");
        return Ok(());
    }

    tracing::info!(%image, "image not found locally — pulling");
    let pull_err = match pull_image(docker, image).await {
        Ok(()) => {
            tracing::info!(%image, "image pulled successfully");
            return Ok(());
        }
        Err(e) => e,
    };

    // Fall back to local build when a Dockerfile is provided.
    if let Some(content) = dockerfile {
        tracing::warn!(
            %image,
            "pull failed, building locally from embedded Dockerfile"
        );
        return build_image_locally(docker, image, content).await;
    }

    Err(pull_err)
}

/// Pull an image from the registry with a timeout.
async fn pull_image(docker: &Docker, image: &str) -> Result<(), ExecutorError> {
    let options = Some(CreateImageOptions {
        from_image: image,
        ..Default::default()
    });

    let pull_result = tokio::time::timeout(IMAGE_TIMEOUT, async {
        let mut stream = docker.create_image(options, None, None);
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(info) => {
                    if let Some(status) = &info.status {
                        tracing::debug!(%image, %status, "pull progress");
                    }
                }
                Err(e) => {
                    return Err(ExecutorError::Infrastructure(format!(
                        "failed to pull image {image}: {e}"
                    )));
                }
            }
        }
        Ok(())
    })
    .await;

    match pull_result {
        Ok(inner) => inner,
        Err(_) => Err(ExecutorError::Infrastructure(format!(
            "image pull timed out after {}s: {image}",
            IMAGE_TIMEOUT.as_secs()
        ))),
    }
}

/// Build an image locally from Dockerfile content using the Docker build API.
///
/// Creates a minimal tar archive containing only the Dockerfile and submits
/// it to the Docker daemon. Bounded by [`IMAGE_TIMEOUT`].
async fn build_image_locally(
    docker: &Docker,
    image: &str,
    dockerfile_content: &str,
) -> Result<(), ExecutorError> {
    let tar = create_dockerfile_tar(dockerfile_content.as_bytes());
    let options = BuildImageOptions {
        t: image.to_string(),
        ..Default::default()
    };

    let build_result = tokio::time::timeout(IMAGE_TIMEOUT, async {
        let mut stream = docker.build_image(options, None, Some(Bytes::from(tar)));
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(BuildInfo {
                    error: Some(error), ..
                }) => {
                    return Err(ExecutorError::Infrastructure(format!(
                        "failed to build image {image}: {error}"
                    )));
                }
                Ok(info) => {
                    if let Some(stream_msg) = &info.stream {
                        let trimmed = stream_msg.trim();
                        if !trimmed.is_empty() {
                            tracing::debug!(%image, msg = %trimmed, "build progress");
                        }
                    }
                }
                Err(e) => {
                    return Err(ExecutorError::Infrastructure(format!(
                        "failed to build image {image}: {e:?}"
                    )));
                }
            }
        }
        Ok(())
    })
    .await;

    match build_result {
        Ok(inner) => {
            inner?;
            tracing::info!(%image, "image built successfully from embedded Dockerfile");
            Ok(())
        }
        Err(_) => Err(ExecutorError::Infrastructure(format!(
            "image build timed out after {}s: {image}",
            IMAGE_TIMEOUT.as_secs()
        ))),
    }
}

/// Create a minimal POSIX (ustar) tar archive containing a single Dockerfile.
///
/// Produces a valid ustar-format tar with one entry named "Dockerfile",
/// followed by two empty 512-byte end-of-archive blocks. No external crate
/// needed.
#[doc(hidden)]
pub fn create_dockerfile_tar(content: &[u8]) -> Vec<u8> {
    let mut tar = Vec::new();
    let mut header = [0u8; 512];

    // Name (100 bytes at offset 0).
    header[..10].copy_from_slice(b"Dockerfile");

    // Mode (8 bytes at offset 100).
    header[100..108].copy_from_slice(b"0000644\0");

    // UID / GID (8 bytes each at offsets 108, 116).
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");

    // Size in octal (12 bytes at offset 124).
    let size_octal = format!("{:011o}\0", content.len());
    header[124..136].copy_from_slice(size_octal.as_bytes());

    // Modification time (12 bytes at offset 136).
    header[136..148].copy_from_slice(b"00000000000\0");

    // Type flag (offset 156) — '0' for regular file.
    header[156] = b'0';

    // USTAR magic (6 bytes at offset 257) + version (2 bytes at offset 263).
    // Required by Docker's Go archive/tar reader.
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");

    // Checksum: fill field with spaces, sum all header bytes, write result.
    header[148..156].copy_from_slice(b"        ");
    let checksum: u32 = header.iter().map(|&b| u32::from(b)).sum();
    let cksum_str = format!("{:06o}\0 ", checksum);
    header[148..156].copy_from_slice(cksum_str.as_bytes());

    tar.extend_from_slice(&header);
    tar.extend_from_slice(content);

    // Pad file data to a 512-byte boundary.
    let remainder = content.len() % 512;
    if remainder > 0 {
        tar.resize(
            tar.len()
                .saturating_add(512_usize.saturating_sub(remainder)),
            0,
        );
    }

    // Two empty 512-byte blocks mark end of archive.
    tar.resize(tar.len().saturating_add(1024), 0);
    tar
}
