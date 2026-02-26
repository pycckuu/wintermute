//! Playwright browser sidecar lifecycle manager.
//!
//! Manages a Docker container running a Flask + Playwright bridge server
//! that exposes browser automation over HTTP. Follows the same sidecar
//! pattern as [`super::egress`] for the Squid proxy.

use std::collections::HashMap;
use std::path::Path;

use bollard::container::{
    Config as ContainerConfig, CreateContainerOptions, InspectContainerOptions,
    RemoveContainerOptions, StartContainerOptions,
};
use bollard::models::{HostConfig, PortBinding, RestartPolicy, RestartPolicyNameEnum};
use bollard::Docker;
use tracing::{info, warn};

use super::ExecutorError;

/// Default Docker image for the Playwright browser sidecar.
pub const BROWSER_IMAGE: &str = "ghcr.io/pycckuu/wintermute-browser:latest";

/// Container name for the browser sidecar.
const CONTAINER_NAME: &str = "wintermute-browser";

/// Port the bridge server listens on inside the container.
const BRIDGE_PORT: u16 = 9222;

/// Memory limit for the browser container (2 GB).
const MEMORY_LIMIT_BYTES: i64 = 2 * 1024 * 1024 * 1024;

/// CPU quota: 2 cores expressed as microseconds per period (200_000 / 100_000).
const CPU_PERIOD: i64 = 100_000;

/// CPU quota microseconds (2 cores).
const CPU_QUOTA: i64 = 200_000;

/// Number of health-check retries before giving up.
const HEALTH_CHECK_RETRIES: u32 = 5;

/// Delay between health-check attempts.
const HEALTH_CHECK_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// HTTP timeout for health-check requests.
const HEALTH_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Embedded Dockerfile for local build fallback when the registry image
/// is unavailable. Based on the official Playwright Python image with
/// Flask and an inline bridge server script.
const BROWSER_DOCKERFILE: &str = r#"FROM mcr.microsoft.com/playwright/python:v1.49.0

RUN pip install --no-cache-dir flask==3.1.*

RUN cat > /opt/browser_bridge.py << 'PYEOF'
import json
from flask import Flask, request, jsonify
from playwright.sync_api import sync_playwright

app = Flask(__name__)
pw = None
browser = None
page = None

MAX_EXTRACT_BYTES = 50 * 1024

def get_browser():
    global pw, browser, page
    if browser is None:
        pw = sync_playwright().start()
        browser = pw.chromium.launch(headless=True, args=[
            "--no-sandbox",
            "--disable-gpu",
            "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE *.com, EXCLUDE *.org, EXCLUDE *.net, EXCLUDE *.io, EXCLUDE *.dev, EXCLUDE *.app, EXCLUDE *.co",
        ])
    if page is None:
        ctx = browser.new_context(viewport={"width": 1280, "height": 720})
        page = ctx.new_page()
    return page

def close_browser():
    global pw, browser, page
    if page is not None:
        try: page.close()
        except Exception: pass
        page = None
    if browser is not None:
        try: browser.close()
        except Exception: pass
        browser = None
    if pw is not None:
        try: pw.stop()
        except Exception: pass
        pw = None

@app.route("/health")
def health():
    return jsonify({"status": "ok"})

@app.route("/execute", methods=["POST"])
def execute():
    try:
        data = request.get_json(force=True)
        action = data.get("action", "")
        timeout_ms = data.get("timeout_ms", 30000)
        p = get_browser()
        p.set_default_timeout(timeout_ms)

        if action == "navigate":
            url = data.get("url", "")
            wait = data.get("wait_for")
            p.goto(url, wait_until="domcontentloaded")
            if wait and wait == "networkidle":
                p.wait_for_load_state("networkidle")
            elif wait:
                p.wait_for_selector(wait, timeout=timeout_ms)
            result = json.dumps({"title": p.title(), "url": p.url})

        elif action == "click":
            sel = data.get("selector", "")
            p.click(sel)
            result = f"clicked {sel}"

        elif action == "type":
            sel = data.get("selector", "")
            text = data.get("text", "")
            p.fill(sel, text)
            result = f"typed into {sel}"

        elif action == "screenshot":
            path = "/workspace/screenshot.png"
            p.screenshot(path=path)
            result = json.dumps({"path": path})

        elif action == "extract":
            sel = data.get("selector")
            if sel:
                el = p.query_selector(sel)
                content = el.inner_text() if el else ""
            else:
                content = p.content()
            if len(content) > MAX_EXTRACT_BYTES:
                content = content[:MAX_EXTRACT_BYTES] + "\n... (truncated)"
            result = content

        elif action == "wait":
            wait_for = data.get("wait_for", "networkidle")
            if wait_for == "networkidle":
                p.wait_for_load_state("networkidle")
            else:
                p.wait_for_selector(wait_for, timeout=timeout_ms)
            result = f"waited for {wait_for}"

        elif action == "scroll":
            direction = data.get("direction", data.get("text", "down"))
            if direction == "up":
                p.evaluate("window.scrollBy(0, -window.innerHeight)")
            else:
                p.evaluate("window.scrollBy(0, window.innerHeight)")
            result = f"scrolled {direction}"

        elif action == "evaluate":
            js = data.get("javascript", "")
            val = p.evaluate(js)
            raw = json.dumps(val) if val is not None else "undefined"
            if len(raw) > MAX_EXTRACT_BYTES:
                raw = raw[:MAX_EXTRACT_BYTES] + "\n... (truncated)"
            result = raw

        elif action == "close":
            close_browser()
            result = "browser closed"

        else:
            return jsonify({"success": False, "error": f"unknown action: {action}"})

        return jsonify({"success": True, "result": result})
    except Exception as e:
        return jsonify({"success": False, "error": f"{type(e).__name__}: {e}"})

if __name__ == "__main__":
    app.run(host="0.0.0.0", port=9222, threaded=True)
PYEOF

USER pwuser
EXPOSE 9222
CMD ["python3", "/opt/browser_bridge.py"]
"#;

/// Playwright browser sidecar providing HTTP-based browser automation.
///
/// The sidecar runs a Flask + Playwright server inside a Docker container,
/// exposing an HTTP API on a localhost port for the host-side Rust binary.
pub struct PlaywrightSidecar {
    base_url: String,
}

impl PlaywrightSidecar {
    /// Ensure the Playwright sidecar container is running and healthy.
    ///
    /// Creates the Docker network (if needed), pulls or builds the image,
    /// creates and starts the container, and verifies the HTTP health endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorError`] if any Docker operation fails or the
    /// health check does not pass within the retry budget.
    pub async fn ensure(
        docker: &Docker,
        image: &str,
        workspace_dir: &Path,
    ) -> Result<Self, ExecutorError> {
        super::ensure_image(docker, image, Some(BROWSER_DOCKERFILE)).await?;

        let workspace_str = workspace_dir.to_str().ok_or_else(|| {
            ExecutorError::Infrastructure("workspace path is not valid UTF-8".to_owned())
        })?;

        ensure_browser_container(docker, image, workspace_str).await?;

        let base_url = format!("http://127.0.0.1:{BRIDGE_PORT}");
        health_check(&base_url).await?;

        Ok(Self { base_url })
    }

    /// Remove the browser sidecar container.
    ///
    /// Silently ignores removal errors (e.g. container already removed).
    pub async fn teardown(docker: &Docker) -> Result<(), ExecutorError> {
        let remove_opts = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        let _ = docker
            .remove_container(CONTAINER_NAME, Some(remove_opts))
            .await;
        Ok(())
    }

    /// Base URL for the HTTP bridge API (e.g. `http://127.0.0.1:9222`).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// Ensure the browser container exists and is running.
///
/// Inspects the container; if it exists and is running, returns immediately.
/// If it exists but is stopped, starts it. If it does not exist, creates
/// and starts it.
async fn ensure_browser_container(
    docker: &Docker,
    image: &str,
    workspace_dir: &str,
) -> Result<(), ExecutorError> {
    let inspect = docker
        .inspect_container(CONTAINER_NAME, None::<InspectContainerOptions>)
        .await;

    let needs_start = match inspect {
        Ok(state) => {
            let running = state.state.and_then(|s| s.running).unwrap_or(false);
            !running
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => {
            create_browser_container(docker, image, workspace_dir).await?;
            true
        }
        Err(e) => {
            return Err(ExecutorError::Infrastructure(format!(
                "failed to inspect browser sidecar: {e}"
            )));
        }
    };

    if needs_start {
        docker
            .start_container(CONTAINER_NAME, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| {
                ExecutorError::Infrastructure(format!("failed to start browser sidecar: {e}"))
            })?;
    }

    Ok(())
}

/// Create the browser sidecar container with resource limits and port mapping.
async fn create_browser_container(
    docker: &Docker,
    image: &str,
    workspace_dir: &str,
) -> Result<(), ExecutorError> {
    let mut labels = HashMap::new();
    labels.insert("wintermute".to_owned(), "true".to_owned());

    let port_key = format!("{BRIDGE_PORT}/tcp");
    let mut port_bindings = HashMap::new();
    port_bindings.insert(
        port_key.clone(),
        Some(vec![PortBinding {
            host_ip: Some("127.0.0.1".to_owned()),
            host_port: Some(BRIDGE_PORT.to_string()),
        }]),
    );

    // The browser sidecar uses the default bridge network (NOT wintermute-net).
    // This prevents the sandbox container from directly reaching the Flask
    // server and bypassing Rust-layer policy enforcement (domain checks,
    // rate limiting, SSRF). The host accesses it via the published port.
    let host_config = HostConfig {
        binds: Some(vec![format!("{workspace_dir}:/workspace")]),
        port_bindings: Some(port_bindings),
        memory: Some(MEMORY_LIMIT_BYTES),
        cpu_period: Some(CPU_PERIOD),
        cpu_quota: Some(CPU_QUOTA),
        restart_policy: Some(RestartPolicy {
            name: Some(RestartPolicyNameEnum::ON_FAILURE),
            maximum_retry_count: Some(5),
        }),
        // Chromium inside Docker needs /dev/shm to be large enough.
        shm_size: Some(512 * 1024 * 1024),
        ..Default::default()
    };

    let mut exposed_ports = HashMap::new();
    exposed_ports.insert(port_key, HashMap::new());

    let container_config = ContainerConfig {
        image: Some(image.to_owned()),
        labels: Some(labels),
        host_config: Some(host_config),
        exposed_ports: Some(exposed_ports),
        // Explicitly empty: no secrets or config leaked into browser sidecar.
        env: Some(Vec::new()),
        ..Default::default()
    };

    let options = Some(CreateContainerOptions {
        name: CONTAINER_NAME.to_owned(),
        platform: None,
    });

    docker
        .create_container(options, container_config)
        .await
        .map_err(|e| {
            ExecutorError::Infrastructure(format!(
                "failed to create browser sidecar container: {e}"
            ))
        })?;

    Ok(())
}

/// Verify the bridge server is responding by polling its `/health` endpoint.
async fn health_check(base_url: &str) -> Result<(), ExecutorError> {
    let client = reqwest::Client::builder()
        .timeout(HEALTH_CHECK_TIMEOUT)
        .build()
        .map_err(|e| {
            ExecutorError::Infrastructure(format!("failed to create health-check client: {e}"))
        })?;

    let url = format!("{base_url}/health");

    for attempt in 1..=HEALTH_CHECK_RETRIES {
        if attempt > 1 {
            tokio::time::sleep(HEALTH_CHECK_DELAY).await;
        }

        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!(attempt, "browser sidecar health check passed");
                return Ok(());
            }
            Ok(resp) => {
                warn!(
                    attempt,
                    status = %resp.status(),
                    "browser sidecar health check returned non-success"
                );
            }
            Err(e) => {
                warn!(attempt, error = %e, "browser sidecar health check failed");
            }
        }
    }

    Err(ExecutorError::Infrastructure(format!(
        "browser sidecar health check failed after {HEALTH_CHECK_RETRIES} retries"
    )))
}
