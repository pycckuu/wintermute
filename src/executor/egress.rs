//! Egress proxy management for Docker sandbox network control.
//!
//! Manages a Squid forward-proxy container that enforces the domain
//! allowlist from `config.toml [egress].allowed_domains`. The sandbox
//! container routes all HTTP(S) traffic through this proxy.

use std::collections::HashMap;

use base64::Engine;
use bollard::container::{
    Config as ContainerConfig, CreateContainerOptions, InspectContainerOptions,
    RemoveContainerOptions, StartContainerOptions,
};
use bollard::models::HostConfig;
use bollard::network::CreateNetworkOptions;
use bollard::Docker;
use tracing::info;

use super::ExecutorError;

const SQUID_IMAGE: &str = "ubuntu/squid:latest";
const SQUID_CONTAINER_NAME: &str = "wintermute-egress";
pub(crate) const NETWORK_NAME: &str = "wintermute-net";
const PROXY_PORT: u16 = 3128;

/// Package registries that are always allowed regardless of user config.
const ALWAYS_ALLOWED_DOMAINS: &[&str] = &[
    // Python
    "pypi.org",
    "files.pythonhosted.org",
    // Node
    "registry.npmjs.org",
    // Rust
    "crates.io",
    "static.crates.io",
    // System packages
    "deb.debian.org",
    "security.debian.org",
    "archive.ubuntu.com",
    "security.ubuntu.com",
];

/// Egress proxy backed by a Squid container on a shared Docker network.
#[derive(Debug, Clone)]
pub struct EgressProxy {
    proxy_address: String,
    network_name: String,
}

impl EgressProxy {
    /// Ensure the egress proxy is running with the given domain allowlist.
    ///
    /// Creates the Docker network and Squid container if they don't exist,
    /// and starts them if they're stopped.
    ///
    /// # Errors
    ///
    /// Returns an error if Docker operations fail.
    pub async fn ensure(
        docker: &Docker,
        allowed_domains: &[String],
    ) -> Result<Self, ExecutorError> {
        ensure_network(docker).await?;
        let squid_config = generate_squid_config(allowed_domains);
        ensure_squid_container(docker, &squid_config).await?;

        Ok(Self {
            proxy_address: format!("{SQUID_CONTAINER_NAME}:{PROXY_PORT}"),
            network_name: NETWORK_NAME.to_owned(),
        })
    }

    /// Returns the proxy address for HTTP_PROXY / HTTPS_PROXY env vars.
    pub fn proxy_address(&self) -> &str {
        &self.proxy_address
    }

    /// Returns the Docker network name that the sandbox should join.
    pub fn network_name(&self) -> &str {
        &self.network_name
    }

    /// Tear down the egress proxy container and network.
    ///
    /// # Errors
    ///
    /// Returns an error if Docker operations fail.
    pub async fn teardown(docker: &Docker) -> Result<(), ExecutorError> {
        let remove_opts = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        // Ignore 404 (already removed).
        let _ = docker
            .remove_container(SQUID_CONTAINER_NAME, Some(remove_opts))
            .await;

        // Remove network (ignore errors — may have other containers attached).
        let _ = docker.remove_network(NETWORK_NAME).await;

        Ok(())
    }
}

/// Generate a Squid configuration that allows only the specified domains
/// plus always-allowed package registries.
#[doc(hidden)]
pub fn generate_squid_config(allowed_domains: &[String]) -> String {
    let mut config = String::new();

    // Basic Squid settings
    config.push_str("# Wintermute egress proxy — auto-generated\n");
    config.push_str("http_port 3128\n\n");

    // Define the allowlist ACL
    config.push_str("# Always-allowed package registries\n");
    for domain in ALWAYS_ALLOWED_DOMAINS {
        config.push_str(&format!("acl wintermute_allowed dstdomain .{domain}\n"));
    }

    config.push_str("\n# User-configured allowed domains\n");
    for domain in allowed_domains {
        let d = domain.trim();
        if !d.is_empty() {
            config.push_str(&format!("acl wintermute_allowed dstdomain .{d}\n"));
        }
    }

    // Access rules
    config.push_str("\n# Allow CONNECT (HTTPS) to allowed domains\n");
    config.push_str("acl SSL_ports port 443\n");
    config.push_str("acl CONNECT method CONNECT\n");
    config.push_str("http_access allow CONNECT wintermute_allowed SSL_ports\n\n");

    config.push_str("# Allow HTTP to allowed domains\n");
    config.push_str("http_access allow wintermute_allowed\n\n");

    config.push_str("# Deny everything else\n");
    config.push_str("http_access deny all\n\n");

    // Logging to stdout for Docker
    config.push_str("# Log to stdout\n");
    config.push_str("access_log stdio:/dev/stdout\n");
    config.push_str("cache_log stdio:/dev/stderr\n\n");

    // Disable caching (we're a forward proxy, not a cache)
    config.push_str("# No disk cache\n");
    config.push_str("cache deny all\n");

    config
}

/// Ensure the `wintermute-net` Docker bridge network exists.
pub(crate) async fn ensure_network(docker: &Docker) -> Result<(), ExecutorError> {
    let inspect = docker.inspect_network::<&str>(NETWORK_NAME, None).await;
    if inspect.is_ok() {
        return Ok(());
    }

    let options = CreateNetworkOptions {
        name: NETWORK_NAME,
        driver: "bridge",
        ..Default::default()
    };

    docker
        .create_network(options)
        .await
        .map_err(|e| ExecutorError::Infrastructure(format!("failed to create network: {e}")))?;

    Ok(())
}

/// Ensure the Squid proxy container is running with the given config.
///
/// If the container exists but its config has drifted (domain allowlist changed),
/// it is recreated with the new config before starting.
async fn ensure_squid_container(docker: &Docker, squid_config: &str) -> Result<(), ExecutorError> {
    let desired_b64 = base64_encode(squid_config);
    let inspect = docker
        .inspect_container(SQUID_CONTAINER_NAME, None::<InspectContainerOptions>)
        .await;

    let needs_start = match inspect {
        Ok(state) => {
            let running = state.state.and_then(|s| s.running).unwrap_or(false);

            // Check if config has drifted by comparing the SQUID_CONFIG_B64 env var.
            let current_b64 = state
                .config
                .as_ref()
                .and_then(|c| c.env.as_ref())
                .and_then(|env| {
                    env.iter()
                        .find(|e| e.starts_with("SQUID_CONFIG_B64="))
                        .map(|e| e.trim_start_matches("SQUID_CONFIG_B64="))
                });

            if current_b64 != Some(desired_b64.as_str()) {
                // Config changed: recreate the container with the new allowlist.
                info!("egress proxy config drifted, recreating container");
                let remove_opts = RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                };
                let _ = docker
                    .remove_container(SQUID_CONTAINER_NAME, Some(remove_opts))
                    .await;
                create_squid_container(docker, squid_config).await?;
                true
            } else {
                !running
            }
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => {
            create_squid_container(docker, squid_config).await?;
            true
        }
        Err(e) => {
            return Err(ExecutorError::Infrastructure(format!(
                "failed to inspect egress proxy: {e}"
            )));
        }
    };

    if needs_start {
        docker
            .start_container(SQUID_CONTAINER_NAME, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| {
                ExecutorError::Infrastructure(format!("failed to start egress proxy: {e}"))
            })?;
    }

    Ok(())
}

/// Create the Squid proxy container.
///
/// Pulls the image first if it is not available locally.
async fn create_squid_container(docker: &Docker, squid_config: &str) -> Result<(), ExecutorError> {
    super::ensure_image(docker, SQUID_IMAGE, None).await?;

    let mut labels = HashMap::new();
    labels.insert("wintermute".to_owned(), "true".to_owned());

    // Pass config via environment variable; entrypoint writes it to disk.
    // Alternative: use a tmpfs mount and write the config file.
    let config_b64 = base64_encode(squid_config);

    let host_config = HostConfig {
        network_mode: Some(NETWORK_NAME.to_owned()),
        restart_policy: Some(bollard::models::RestartPolicy {
            name: Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED),
            maximum_retry_count: None,
        }),
        ..Default::default()
    };

    let container_config = ContainerConfig {
        image: Some(SQUID_IMAGE.to_owned()),
        labels: Some(labels),
        env: Some(vec![format!("SQUID_CONFIG_B64={config_b64}")]),
        cmd: Some(vec![
            "bash".to_owned(),
            "-c".to_owned(),
            "echo \"$SQUID_CONFIG_B64\" | base64 -d > /etc/squid/squid.conf && squid -NYC"
                .to_owned(),
        ]),
        host_config: Some(host_config),
        ..Default::default()
    };

    let options = Some(CreateContainerOptions {
        name: SQUID_CONTAINER_NAME.to_owned(),
        platform: None,
    });

    docker
        .create_container(options, container_config)
        .await
        .map_err(|e| {
            ExecutorError::Infrastructure(format!("failed to create egress proxy container: {e}"))
        })?;

    Ok(())
}

/// Encode a string as standard base64.
fn base64_encode(input: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(input)
}
