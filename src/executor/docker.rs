//! Docker-backed sandbox executor with hardening defaults.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bollard::container::{
    Config as ContainerConfig, CreateContainerOptions, InspectContainerOptions,
    RemoveContainerOptions, StartContainerOptions,
};
use bollard::errors::Error as BollardError;
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::models::HostConfig;
use bollard::Docker;
use tokio_stream::StreamExt;

use crate::config::{Config, RuntimePaths};

use super::redactor::Redactor;
use super::{ExecOptions, ExecResult, Executor, ExecutorError, ExecutorKind, HealthStatus};

const SANDBOX_IMAGE: &str = "wintermute-sandbox:latest";
const SANDBOX_CONTAINER_NAME: &str = "wintermute-sandbox";
const RESET_REQUIREMENTS_COMMAND: &str =
    "if [ -f /scripts/requirements.txt ]; then pip install --user -r /scripts/requirements.txt; fi";

/// Docker-backed executor implementation.
#[derive(Debug, Clone)]
pub struct DockerExecutor {
    docker: Docker,
    container_name: String,
    scripts_dir: PathBuf,
    workspace_dir: PathBuf,
    redactor: Redactor,
}

impl DockerExecutor {
    /// Create, configure, and warm the sandbox container.
    ///
    /// # Errors
    ///
    /// Returns an error when Docker cannot be reached or container provisioning fails.
    pub async fn new(
        config: &Config,
        paths: &RuntimePaths,
        redactor: Redactor,
    ) -> Result<Self, ExecutorError> {
        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| ExecutorError::Infrastructure(e.to_string()))?;

        let workspace_dir = paths.workspace_dir.clone();
        let scripts_dir = paths.scripts_dir.clone();
        std::fs::create_dir_all(&workspace_dir)
            .map_err(|e| ExecutorError::Infrastructure(e.to_string()))?;
        std::fs::create_dir_all(&scripts_dir)
            .map_err(|e| ExecutorError::Infrastructure(e.to_string()))?;

        let instance = Self {
            docker,
            container_name: SANDBOX_CONTAINER_NAME.to_owned(),
            scripts_dir,
            workspace_dir,
            redactor,
        };
        instance.ensure_container(config).await?;
        Ok(instance)
    }

    /// Returns true if Docker daemon is available.
    pub async fn docker_available() -> bool {
        let connected = Docker::connect_with_local_defaults();
        match connected {
            Ok(docker) => docker.ping().await.is_ok(),
            Err(_) => false,
        }
    }

    /// Recreate sandbox container and reinstall runtime requirements.
    ///
    /// # Errors
    ///
    /// Returns an error if container recreation fails.
    pub async fn reset_container(&self, config: &Config) -> Result<(), ExecutorError> {
        let remove_opts = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        let _ = self
            .docker
            .remove_container(&self.container_name, Some(remove_opts))
            .await;

        self.ensure_container(config).await?;

        let reset_opts = ExecOptions {
            timeout: Duration::from_secs(600),
            working_dir: Some(PathBuf::from("/workspace")),
        };
        let _ = self.execute(RESET_REQUIREMENTS_COMMAND, reset_opts).await?;
        Ok(())
    }

    async fn ensure_container(&self, config: &Config) -> Result<(), ExecutorError> {
        let inspect = self
            .docker
            .inspect_container(&self.container_name, None::<InspectContainerOptions>)
            .await;

        match inspect {
            Ok(state) => {
                let running = state.state.and_then(|state| state.running).unwrap_or(false);
                if !running {
                    self.docker
                        .start_container(
                            &self.container_name,
                            None::<StartContainerOptions<String>>,
                        )
                        .await
                        .map_err(|e| ExecutorError::Infrastructure(e.to_string()))?;
                }
                Ok(())
            }
            Err(BollardError::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                self.create_container(config).await?;
                self.docker
                    .start_container(&self.container_name, None::<StartContainerOptions<String>>)
                    .await
                    .map_err(|e| ExecutorError::Infrastructure(e.to_string()))
            }
            Err(err) => Err(ExecutorError::Infrastructure(err.to_string())),
        }
    }

    async fn create_container(&self, config: &Config) -> Result<(), ExecutorError> {
        let container_config =
            build_container_config(&self.workspace_dir, &self.scripts_dir, config)?;

        let options = Some(CreateContainerOptions {
            name: self.container_name.clone(),
            platform: None,
        });

        self.docker
            .create_container(options, container_config)
            .await
            .map_err(|e| ExecutorError::Infrastructure(e.to_string()))?;

        Ok(())
    }

    async fn collect_exec_output(&self, exec_id: &str) -> Result<(String, String), ExecutorError> {
        let started = self
            .docker
            .start_exec(
                exec_id,
                Some(StartExecOptions {
                    detach: false,
                    tty: false,
                    output_capacity: None,
                }),
            )
            .await
            .map_err(|e| ExecutorError::Infrastructure(e.to_string()))?;

        let mut stdout = String::new();
        let mut stderr = String::new();

        if let StartExecResults::Attached { mut output, .. } = started {
            while let Some(chunk) = output.next().await {
                let log = chunk.map_err(|e| ExecutorError::Infrastructure(e.to_string()))?;
                match log {
                    bollard::container::LogOutput::StdOut { message } => {
                        stdout.push_str(&String::from_utf8_lossy(&message));
                    }
                    bollard::container::LogOutput::StdErr { message } => {
                        stderr.push_str(&String::from_utf8_lossy(&message));
                    }
                    bollard::container::LogOutput::Console { message } => {
                        stdout.push_str(&String::from_utf8_lossy(&message));
                    }
                    _ => {}
                }
            }
        }

        Ok((stdout, stderr))
    }
}

#[async_trait::async_trait]
impl Executor for DockerExecutor {
    async fn execute(&self, command: &str, opts: ExecOptions) -> Result<ExecResult, ExecutorError> {
        let timeout_secs = opts.timeout.as_secs().max(1);
        let wrapped_command = format!(
            "timeout --signal=TERM --kill-after=5 {timeout_secs} bash -lc {}",
            shell_quote(command)
        );

        let working_dir = opts
            .working_dir
            .and_then(|value| value.to_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "/workspace".to_owned());

        let create_exec = CreateExecOptions {
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            cmd: Some(vec!["bash".to_owned(), "-lc".to_owned(), wrapped_command]),
            env: Some(Vec::new()),
            working_dir: Some(working_dir),
            ..Default::default()
        };

        let created = self
            .docker
            .create_exec(&self.container_name, create_exec)
            .await
            .map_err(|e| ExecutorError::Infrastructure(e.to_string()))?;

        let wait_window = opts.timeout.saturating_add(Duration::from_secs(10));
        let output_result =
            tokio::time::timeout(wait_window, self.collect_exec_output(&created.id)).await;

        let (stdout_raw, stderr_raw) = match output_result {
            Ok(result) => result?,
            Err(_) => {
                return Err(ExecutorError::Timeout {
                    seconds: wait_window.as_secs(),
                });
            }
        };

        let inspect = self
            .docker
            .inspect_exec(&created.id)
            .await
            .map_err(|e| ExecutorError::Infrastructure(e.to_string()))?;
        let exit_code = inspect.exit_code.unwrap_or(-1);

        let stdout = self.redactor.redact(&stdout_raw);
        let stderr = self.redactor.redact(&stderr_raw);
        Ok(ExecResult {
            exit_code,
            stdout,
            stderr,
        })
    }

    async fn health_check(&self) -> Result<HealthStatus, ExecutorError> {
        self.docker
            .ping()
            .await
            .map_err(|e| ExecutorError::Infrastructure(e.to_string()))?;

        let inspect = self
            .docker
            .inspect_container(&self.container_name, None::<InspectContainerOptions>)
            .await
            .map_err(|e| ExecutorError::Infrastructure(e.to_string()))?;

        let running = inspect
            .state
            .and_then(|value| value.running)
            .unwrap_or(false);
        let details = if running {
            "docker sandbox is running".to_owned()
        } else {
            "docker sandbox exists but is not running".to_owned()
        };

        Ok(HealthStatus {
            is_healthy: running,
            kind: ExecutorKind::Docker,
            details,
        })
    }

    fn has_network_isolation(&self) -> bool {
        true
    }

    fn scripts_dir(&self) -> &Path {
        &self.scripts_dir
    }

    fn workspace_dir(&self) -> &Path {
        &self.workspace_dir
    }

    fn kind(&self) -> ExecutorKind {
        ExecutorKind::Docker
    }
}

fn build_container_config(
    workspace_dir: &Path,
    scripts_dir: &Path,
    config: &Config,
) -> Result<ContainerConfig<String>, ExecutorError> {
    let memory_limit = i64::from(config.sandbox.memory_mb)
        .saturating_mul(1024)
        .saturating_mul(1024);

    let cpu_limit = f64_to_nano_cpu(config.sandbox.cpu_cores)?;

    let mut tmpfs: HashMap<String, String> = HashMap::new();
    tmpfs.insert("/tmp".to_owned(), "rw,size=512m".to_owned());

    let host_config = HostConfig {
        network_mode: Some("none".to_owned()),
        readonly_rootfs: Some(true),
        cap_drop: Some(vec!["ALL".to_owned()]),
        pids_limit: Some(256),
        memory: Some(memory_limit),
        nano_cpus: Some(cpu_limit),
        binds: Some(vec![
            format!("{}:/workspace", workspace_dir.display()),
            format!("{}:/scripts", scripts_dir.display()),
        ]),
        tmpfs: Some(tmpfs),
        ..Default::default()
    };

    Ok(ContainerConfig {
        image: Some(SANDBOX_IMAGE.to_owned()),
        cmd: Some(vec!["sleep".to_owned(), "infinity".to_owned()]),
        user: Some("wintermute".to_owned()),
        working_dir: Some("/workspace".to_owned()),
        env: Some(Vec::new()),
        host_config: Some(host_config),
        ..Default::default()
    })
}

fn shell_quote(raw: &str) -> String {
    let escaped = raw.replace('\'', r"'\''");
    format!("'{escaped}'")
}

fn f64_to_nano_cpu(cpu_cores: f64) -> Result<i64, ExecutorError> {
    if !cpu_cores.is_finite() || cpu_cores <= 0.0 {
        return Err(ExecutorError::Infrastructure(
            "cpu_cores must be a positive finite number".to_owned(),
        ));
    }

    let rendered = format!("{cpu_cores:.9}");
    let mut parts = rendered.split('.');
    let whole_part_raw = parts.next().unwrap_or("0");
    let fraction_part_raw = parts.next().unwrap_or("0");

    let whole_part = whole_part_raw
        .parse::<i64>()
        .map_err(|e| ExecutorError::Infrastructure(e.to_string()))?;
    let mut fraction = fraction_part_raw.to_owned();
    while fraction.len() < 9 {
        fraction.push('0');
    }
    if fraction.len() > 9 {
        fraction.truncate(9);
    }
    let fractional_part = fraction
        .parse::<i64>()
        .map_err(|e| ExecutorError::Infrastructure(e.to_string()))?;

    let nanos = whole_part
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(fractional_part))
        .ok_or_else(|| {
            ExecutorError::Infrastructure("cpu_cores exceed supported range".to_owned())
        })?;

    if nanos <= 0 {
        return Err(ExecutorError::Infrastructure(
            "cpu_cores converted to non-positive nano CPU value".to_owned(),
        ));
    }
    Ok(nanos)
}
