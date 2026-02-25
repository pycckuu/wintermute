//! Egress proxy management for Docker sandbox network control.
//!
//! Manages a Squid forward-proxy container that enforces the domain
//! allowlist from `config.toml [egress].allowed_domains`. The sandbox
//! container routes all HTTP(S) traffic through this proxy.

use std::collections::HashMap;

use bollard::container::{
    Config as ContainerConfig, CreateContainerOptions, InspectContainerOptions,
    RemoveContainerOptions, StartContainerOptions,
};
use bollard::models::HostConfig;
use bollard::network::CreateNetworkOptions;
use bollard::Docker;

use super::ExecutorError;

const SQUID_IMAGE: &str = "ubuntu/squid:latest";
const SQUID_CONTAINER_NAME: &str = "wintermute-egress";
const NETWORK_NAME: &str = "wintermute-net";
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
async fn ensure_network(docker: &Docker) -> Result<(), ExecutorError> {
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
async fn ensure_squid_container(docker: &Docker, squid_config: &str) -> Result<(), ExecutorError> {
    let inspect = docker
        .inspect_container(SQUID_CONTAINER_NAME, None::<InspectContainerOptions>)
        .await;

    let needs_start = match inspect {
        Ok(state) => {
            let running = state.state.and_then(|s| s.running).unwrap_or(false);
            !running
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
async fn create_squid_container(docker: &Docker, squid_config: &str) -> Result<(), ExecutorError> {
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

/// Simple base64 encoding without pulling in a crate.
fn base64_encode(input: &str) -> String {
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut encoder = Base64Encoder::new(&mut buf);
        // Write cannot fail: the underlying Vec<u8> is infallible.
        let _ = encoder.write_all(input.as_bytes());
        encoder.finish();
    }
    // Safety: Base64 output is always ASCII, so this conversion is infallible.
    String::from_utf8(buf).unwrap_or_default()
}

/// Minimal base64 encoder (no external dependency).
struct Base64Encoder<'a> {
    out: &'a mut Vec<u8>,
    buf: [u8; 3],
    buf_len: usize,
}

const B64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

impl<'a> Base64Encoder<'a> {
    fn new(out: &'a mut Vec<u8>) -> Self {
        Self {
            out,
            buf: [0; 3],
            buf_len: 0,
        }
    }

    fn flush_triplet(&mut self) {
        let b0 = self.buf[0];
        let b1 = self.buf[1];
        let b2 = self.buf[2];

        self.out.push(B64_CHARS[(b0 >> 2) as usize]);
        self.out
            .push(B64_CHARS[((b0 & 0x03) << 4 | (b1 >> 4)) as usize]);

        if self.buf_len > 1 {
            self.out
                .push(B64_CHARS[((b1 & 0x0f) << 2 | (b2 >> 6)) as usize]);
        } else {
            self.out.push(b'=');
        }

        if self.buf_len > 2 {
            self.out.push(B64_CHARS[(b2 & 0x3f) as usize]);
        } else {
            self.out.push(b'=');
        }

        self.buf = [0; 3];
        self.buf_len = 0;
    }

    fn finish(&mut self) {
        if self.buf_len > 0 {
            self.flush_triplet();
        }
    }
}

impl<'a> std::io::Write for Base64Encoder<'a> {
    #[allow(clippy::arithmetic_side_effects)]
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        for &byte in data {
            self.buf[self.buf_len] = byte;
            self.buf_len += 1;
            if self.buf_len == 3 {
                self.flush_triplet();
            }
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
