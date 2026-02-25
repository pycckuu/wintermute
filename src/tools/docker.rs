//! Docker container management tool (`docker_manage`).
//!
//! Provides host-side Docker operations via the bollard API.
//! All container-targeting actions enforce the `wintermute=true` label policy
//! to prevent the agent from managing unrelated containers.

use std::collections::HashMap;

use bollard::container::{
    Config as ContainerConfig, CreateContainerOptions, InspectContainerOptions,
    ListContainersOptions, LogsOptions, RemoveContainerOptions, StartContainerOptions,
    StopContainerOptions,
};
use bollard::image::CreateImageOptions;
use bollard::models::HostConfig;
use bollard::network::CreateNetworkOptions;
use bollard::Docker;
use serde_json::Value;
use tokio_stream::StreamExt;

use crate::providers::ToolDefinition;

use super::ToolError;

/// Label key applied to all agent-managed containers.
pub const WINTERMUTE_LABEL: &str = "wintermute";

/// Execute a docker_manage action.
///
/// # Errors
///
/// Returns `ToolError` on invalid input, missing label, or Docker API failures.
pub async fn docker_manage(docker: &Docker, input: &Value) -> Result<String, ToolError> {
    let action = input
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: action".to_owned()))?;

    match action {
        "run" => action_run(docker, input).await,
        "stop" => action_stop(docker, input).await,
        "rm" => action_rm(docker, input).await,
        "ps" => action_ps(docker).await,
        "logs" => action_logs(docker, input).await,
        "pull" => action_pull(docker, input).await,
        "network_create" => action_network_create(docker, input).await,
        "network_connect" => action_network_connect(docker, input).await,
        "exec" => action_exec(docker, input).await,
        "inspect" => action_inspect(docker, input).await,
        _ => Err(ToolError::InvalidInput(format!(
            "unknown docker_manage action: {action}"
        ))),
    }
}

/// Return the tool definition for `docker_manage`.
pub fn docker_manage_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "docker_manage".to_owned(),
        description: "Manage Docker containers and services on the host. Run, stop, pull, logs, exec. For spinning up services the agent needs.".to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["run", "stop", "rm", "ps", "logs", "pull",
                             "network_create", "network_connect", "exec", "inspect"],
                    "description": "Docker action to perform"
                },
                "image": {
                    "type": "string",
                    "description": "Image name for run/pull"
                },
                "container": {
                    "type": "string",
                    "description": "Container name/ID for stop/rm/logs/exec/inspect"
                },
                "args": {
                    "type": "object",
                    "description": "Additional arguments: name, ports, volumes, env, network, command, tail, etc."
                }
            },
            "required": ["action"]
        }),
    }
}

// ---------------------------------------------------------------------------
// Label verification
// ---------------------------------------------------------------------------

/// Verify that a container has the `wintermute=true` label.
async fn verify_wintermute_label(docker: &Docker, container: &str) -> Result<(), ToolError> {
    let inspect = docker
        .inspect_container(container, None::<InspectContainerOptions>)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to inspect container: {e}")))?;

    let has_label = inspect
        .config
        .as_ref()
        .and_then(|c| c.labels.as_ref())
        .and_then(|labels| labels.get(WINTERMUTE_LABEL))
        .is_some_and(|v| v == "true");

    if !has_label {
        return Err(ToolError::ExecutionFailed(format!(
            "container '{container}' is not managed by wintermute (missing wintermute=true label)"
        )));
    }

    Ok(())
}

/// Extract the `container` field from tool input.
fn get_container_name(input: &Value) -> Result<&str, ToolError> {
    input
        .get("container")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: container".to_owned()))
}

/// Extract the `args` object from tool input, defaulting to an empty object.
fn get_args(input: &Value) -> &Value {
    static EMPTY: std::sync::LazyLock<Value> =
        std::sync::LazyLock::new(|| Value::Object(serde_json::Map::new()));
    input.get("args").unwrap_or(&EMPTY)
}

/// Extract a JSON array of strings from a key within `args`, defaulting to empty.
fn get_string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// Run a new container from an image, applying the `wintermute=true` label.
async fn action_run(docker: &Docker, input: &Value) -> Result<String, ToolError> {
    let image = input
        .get("image")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("run requires 'image' field".to_owned()))?;

    let args = get_args(input);

    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(image.split('/').next_back().unwrap_or(image));

    // Always apply wintermute label.
    let mut labels = HashMap::new();
    labels.insert(WINTERMUTE_LABEL.to_owned(), "true".to_owned());

    // Port bindings
    let ports = get_string_array(args, "ports");

    let port_bindings = if ports.is_empty() {
        None
    } else {
        let mut bindings = HashMap::new();
        for mapping in &ports {
            if let Some((host_port, container_port)) = mapping.split_once(':') {
                bindings.insert(
                    format!("{container_port}/tcp"),
                    Some(vec![bollard::models::PortBinding {
                        host_ip: Some("0.0.0.0".to_owned()),
                        host_port: Some(host_port.to_owned()),
                    }]),
                );
            }
        }
        Some(bindings)
    };

    // Volume mounts
    let volumes = get_string_array(args, "volumes");

    // Env vars
    let env = get_string_array(args, "env");

    // Network
    let network = args.get("network").and_then(|v| v.as_str());

    // Restart policy
    let restart = args.get("restart").and_then(|v| v.as_str());
    let restart_policy = restart.map(|r| bollard::models::RestartPolicy {
        name: match r {
            "always" => Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
            "unless-stopped" => Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED),
            "on-failure" => Some(bollard::models::RestartPolicyNameEnum::ON_FAILURE),
            _ => Some(bollard::models::RestartPolicyNameEnum::NO),
        },
        maximum_retry_count: None,
    });

    let host_config = HostConfig {
        port_bindings,
        binds: if volumes.is_empty() {
            None
        } else {
            Some(volumes)
        },
        network_mode: network.map(ToOwned::to_owned),
        restart_policy,
        ..Default::default()
    };

    let container_config = ContainerConfig {
        image: Some(image.to_owned()),
        labels: Some(labels),
        env: if env.is_empty() { None } else { Some(env) },
        host_config: Some(host_config),
        ..Default::default()
    };

    let options = Some(CreateContainerOptions {
        name: name.to_owned(),
        platform: None,
    });

    let created = docker
        .create_container(options, container_config)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to create container: {e}")))?;

    docker
        .start_container(&created.id, None::<StartContainerOptions<String>>)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to start container: {e}")))?;

    Ok(format!(
        "Container '{}' started (id: {})",
        name,
        &created.id[..12.min(created.id.len())]
    ))
}

/// Stop a wintermute-labeled container with a 10-second grace period.
async fn action_stop(docker: &Docker, input: &Value) -> Result<String, ToolError> {
    let container = get_container_name(input)?;
    verify_wintermute_label(docker, container).await?;

    docker
        .stop_container(container, Some(StopContainerOptions { t: 10 }))
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to stop container: {e}")))?;

    Ok(format!("Container '{container}' stopped"))
}

/// Force-remove a wintermute-labeled container.
async fn action_rm(docker: &Docker, input: &Value) -> Result<String, ToolError> {
    let container = get_container_name(input)?;
    verify_wintermute_label(docker, container).await?;

    let opts = RemoveContainerOptions {
        force: true,
        ..Default::default()
    };
    docker
        .remove_container(container, Some(opts))
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to remove container: {e}")))?;

    Ok(format!("Container '{container}' removed"))
}

/// List all containers with the `wintermute=true` label.
async fn action_ps(docker: &Docker) -> Result<String, ToolError> {
    let options = Some(ListContainersOptions {
        all: true,
        filters: HashMap::from([("label".to_owned(), vec!["wintermute=true".to_owned()])]),
        ..Default::default()
    });

    let containers = docker
        .list_containers(options)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to list containers: {e}")))?;

    if containers.is_empty() {
        return Ok("No wintermute-managed containers running.".to_owned());
    }

    let mut output = String::new();
    for c in &containers {
        let name = c
            .names
            .as_ref()
            .and_then(|n| n.first())
            .map(|n| n.trim_start_matches('/'))
            .unwrap_or("unknown");
        let image = c.image.as_deref().unwrap_or("unknown");
        let state = c.state.as_deref().unwrap_or("unknown");
        let status = c.status.as_deref().unwrap_or("");
        output.push_str(&format!("{name}\t{image}\t{state}\t{status}\n"));
    }

    Ok(output)
}

/// Fetch recent logs from a wintermute-labeled container.
async fn action_logs(docker: &Docker, input: &Value) -> Result<String, ToolError> {
    let container = get_container_name(input)?;
    verify_wintermute_label(docker, container).await?;

    let args = get_args(input);
    let tail = args.get("tail").and_then(|v| v.as_str()).unwrap_or("100");

    let options = LogsOptions::<String> {
        stdout: true,
        stderr: true,
        tail: tail.to_owned(),
        ..Default::default()
    };

    let mut stream = docker.logs(container, Some(options));
    let mut output = String::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(log) => output.push_str(&log.to_string()),
            Err(e) => {
                return Err(ToolError::ExecutionFailed(format!(
                    "failed to read logs: {e}"
                )));
            }
        }
    }

    Ok(output)
}

/// Pull a Docker image from a registry.
async fn action_pull(docker: &Docker, input: &Value) -> Result<String, ToolError> {
    let image = input
        .get("image")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("pull requires 'image' field".to_owned()))?;

    let options = Some(CreateImageOptions {
        from_image: image,
        ..Default::default()
    });

    let mut stream = docker.create_image(options, None, None);
    let mut last_status = String::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(info) => {
                if let Some(status) = &info.status {
                    last_status = status.clone();
                }
            }
            Err(e) => {
                return Err(ToolError::ExecutionFailed(format!(
                    "failed to pull image: {e}"
                )));
            }
        }
    }

    Ok(format!(
        "Image '{image}' pulled. Last status: {last_status}"
    ))
}

/// Create a Docker bridge network with the `wintermute=true` label.
async fn action_network_create(docker: &Docker, input: &Value) -> Result<String, ToolError> {
    let args = get_args(input);
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("network_create requires args.name".to_owned()))?;

    let mut labels = HashMap::new();
    labels.insert(WINTERMUTE_LABEL, "true");

    let options = CreateNetworkOptions {
        name,
        driver: "bridge",
        labels,
        ..Default::default()
    };

    docker
        .create_network(options)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to create network: {e}")))?;

    Ok(format!("Network '{name}' created"))
}

/// Connect a wintermute-labeled container to a network.
async fn action_network_connect(docker: &Docker, input: &Value) -> Result<String, ToolError> {
    let container = get_container_name(input)?;
    verify_wintermute_label(docker, container).await?;

    let args = get_args(input);
    let network = args
        .get("network")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ToolError::InvalidInput("network_connect requires args.network".to_owned())
        })?;

    let connect_opts = bollard::network::ConnectNetworkOptions {
        container: container.to_owned(),
        ..Default::default()
    };

    docker
        .connect_network(network, connect_opts)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to connect network: {e}")))?;

    Ok(format!(
        "Container '{container}' connected to network '{network}'"
    ))
}

/// Execute a command inside a running wintermute-labeled container.
async fn action_exec(docker: &Docker, input: &Value) -> Result<String, ToolError> {
    let container = get_container_name(input)?;
    verify_wintermute_label(docker, container).await?;

    let args = get_args(input);
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("exec requires args.command".to_owned()))?;

    let exec_opts = bollard::exec::CreateExecOptions {
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        cmd: Some(vec!["bash".to_owned(), "-c".to_owned(), command.to_owned()]),
        ..Default::default()
    };

    let created = docker
        .create_exec(container, exec_opts)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to create exec: {e}")))?;

    let started = docker
        .start_exec(
            &created.id,
            Some(bollard::exec::StartExecOptions {
                detach: false,
                tty: false,
                output_capacity: None,
            }),
        )
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to start exec: {e}")))?;

    let mut output = String::new();
    if let bollard::exec::StartExecResults::Attached {
        output: mut exec_stream,
        ..
    } = started
    {
        while let Some(chunk) = exec_stream.next().await {
            match chunk {
                Ok(log) => {
                    output.push_str(&log.to_string());
                }
                Err(e) => {
                    return Err(ToolError::ExecutionFailed(format!(
                        "failed to read exec output: {e}"
                    )));
                }
            }
        }
    }

    Ok(output)
}

/// Inspect a wintermute-labeled container, returning full JSON details.
async fn action_inspect(docker: &Docker, input: &Value) -> Result<String, ToolError> {
    let container = get_container_name(input)?;
    verify_wintermute_label(docker, container).await?;

    let info = docker
        .inspect_container(container, None::<InspectContainerOptions>)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to inspect container: {e}")))?;

    serde_json::to_string_pretty(&info)
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to serialize inspect: {e}")))
}
