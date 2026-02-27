//! WhatsApp setup: container lifecycle and QR code linking.
//!
//! Manages the `wintermute-whatsapp` Docker sidecar container using the same
//! inspect-start-create pattern as [`crate::executor::playwright`].

use std::collections::HashMap;

use bollard::container::{
    Config as ContainerConfig, CreateContainerOptions, StartContainerOptions,
};
use bollard::image::CreateImageOptions;
use bollard::models::{HostConfig, PortBinding, RestartPolicy, RestartPolicyNameEnum};
use bollard::Docker;
use tokio_stream::StreamExt;
use tracing::{info, warn};

use super::client::WhatsAppClient;
use super::WhatsAppError;

/// Container name for the WhatsApp sidecar.
pub const CONTAINER_NAME: &str = "wintermute-whatsapp";

/// Default bridge port.
pub const BRIDGE_PORT: u16 = 3001;

/// Memory limit for the WhatsApp sidecar (512 MB).
const MEMORY_LIMIT_BYTES: i64 = 512 * 1024 * 1024;

/// Ensure the WhatsApp sidecar container is running.
///
/// Follows the same pattern as `PlaywrightSidecar::ensure()`:
/// inspect -> start if stopped -> create if missing.
pub async fn ensure_container(docker: &Docker, image: &str) -> Result<(), WhatsAppError> {
    // Step 1: Check if container already exists
    match docker.inspect_container(CONTAINER_NAME, None).await {
        Ok(info) => {
            let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
            if running {
                info!(
                    container = CONTAINER_NAME,
                    "WhatsApp sidecar already running"
                );
                return Ok(());
            }
            // Container exists but is stopped — start it.
            docker
                .start_container(CONTAINER_NAME, None::<StartContainerOptions<String>>)
                .await
                .map_err(|e| {
                    WhatsAppError::SetupFailed(format!("failed to start container: {e}"))
                })?;
            info!(container = CONTAINER_NAME, "WhatsApp sidecar started");
            return Ok(());
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => {
            // Container doesn't exist — fall through to create it.
        }
        Err(e) => {
            return Err(WhatsAppError::SetupFailed(format!(
                "failed to inspect container: {e}"
            )));
        }
    }

    // Step 2: Pull the image
    let pull_opts = CreateImageOptions {
        from_image: image,
        ..Default::default()
    };
    let mut pull_stream = docker.create_image(Some(pull_opts), None, None);
    while let Some(result) = pull_stream.next().await {
        if let Err(e) = result {
            warn!(error = %e, "image pull warning");
        }
    }
    info!(image, "WhatsApp sidecar image pulled");

    // Step 3: Create container with port binding and resource limits
    let port_key = format!("{BRIDGE_PORT}/tcp");
    let mut port_bindings = HashMap::new();
    port_bindings.insert(
        port_key.clone(),
        Some(vec![PortBinding {
            host_ip: Some("127.0.0.1".to_owned()),
            host_port: Some(BRIDGE_PORT.to_string()),
        }]),
    );

    // NOTE: The WhatsApp sidecar intentionally runs on the default Docker bridge
    // network rather than the egress-proxied wintermute-net. The sidecar must
    // maintain a persistent WebSocket connection to WhatsApp servers, which is
    // incompatible with HTTP-only egress proxying. The sidecar binds only to
    // 127.0.0.1 and exposes no shell access.
    let host_config = HostConfig {
        port_bindings: Some(port_bindings),
        restart_policy: Some(RestartPolicy {
            name: Some(RestartPolicyNameEnum::ON_FAILURE),
            maximum_retry_count: Some(5),
        }),
        memory: Some(MEMORY_LIMIT_BYTES),
        ..Default::default()
    };

    let mut labels = HashMap::new();
    labels.insert("wintermute".to_owned(), "true".to_owned());

    let mut exposed_ports = HashMap::new();
    exposed_ports.insert(port_key, HashMap::new());

    let container_config = ContainerConfig {
        image: Some(image.to_owned()),
        labels: Some(labels),
        exposed_ports: Some(exposed_ports),
        host_config: Some(host_config),
        ..Default::default()
    };

    let create_opts = CreateContainerOptions {
        name: CONTAINER_NAME.to_owned(),
        platform: None,
    };
    docker
        .create_container(Some(create_opts), container_config)
        .await
        .map_err(|e| WhatsAppError::SetupFailed(format!("failed to create container: {e}")))?;

    docker
        .start_container(CONTAINER_NAME, None::<StartContainerOptions<String>>)
        .await
        .map_err(|e| WhatsAppError::SetupFailed(format!("failed to start container: {e}")))?;

    info!(
        container = CONTAINER_NAME,
        image, "WhatsApp sidecar created and started"
    );
    Ok(())
}

/// Run the QR setup flow: ensure container, wait for health, return QR code.
pub async fn setup_qr(docker: &Docker, image: &str) -> Result<String, WhatsAppError> {
    ensure_container(docker, image).await?;

    let client = WhatsAppClient::with_port(BRIDGE_PORT);
    client.wait_healthy().await?;

    client.get_qr().await
}
