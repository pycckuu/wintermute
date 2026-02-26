//! Playwright browser sidecar lifecycle manager.
//!
//! Manages a Docker container running a Flask + Playwright bridge server
//! that exposes browser automation over HTTP. Follows the same sidecar
//! pattern as [`super::egress`] for the Squid proxy.

use std::collections::HashMap;
use std::path::Path;

use base64::Engine;
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

/// Port the bridge server listens on inside the container, published to the host.
pub const BRIDGE_PORT: u16 = 9223;

/// Memory limit for the browser container (2 GB).
const MEMORY_LIMIT_BYTES: i64 = 2 * 1024 * 1024 * 1024;

/// CFS scheduling period in microseconds.
const CPU_PERIOD: i64 = 100_000;

/// CFS quota in microseconds (200_000 / 100_000 = 2 cores).
const CPU_QUOTA: i64 = 200_000;

/// Number of health-check retries before giving up.
const HEALTH_CHECK_RETRIES: u32 = 5;

/// Delay between health-check attempts.
const HEALTH_CHECK_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// HTTP timeout for health-check requests.
const HEALTH_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Python bridge server script embedded as a constant.
///
/// Base64-encoded into the Dockerfile at runtime to avoid heredoc issues
/// with Docker's classic builder (which ends `RUN` at the first newline).
pub const BRIDGE_SCRIPT: &str = r#"import json, os
from flask import Flask, request, jsonify
from playwright.sync_api import sync_playwright

app = Flask(__name__)
pw = None
browser = None
page = None

MAX_EXTRACT_BYTES = 50 * 1024

# When CDP_TARGET is set, connect to an external Chrome instance instead
# of launching headless Chromium. Used for attached mode (user's browser).
CDP_TARGET = os.environ.get("CDP_TARGET")

def get_browser():
    global pw, browser, page
    if browser is None:
        pw = sync_playwright().start()
        if CDP_TARGET:
            browser = pw.chromium.connect_over_cdp(CDP_TARGET)
        else:
            browser = pw.chromium.launch(headless=True, args=[
                "--no-sandbox",
                "--disable-gpu",
                "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE *.com, EXCLUDE *.org, EXCLUDE *.net, EXCLUDE *.io, EXCLUDE *.dev, EXCLUDE *.app, EXCLUDE *.co",
            ])
    if page is None:
        if CDP_TARGET:
            # In attached mode, use an existing page from the user's browser
            # if any are open, otherwise create a new context and page.
            contexts = browser.contexts
            if contexts and contexts[0].pages:
                page = contexts[0].pages[0]
            else:
                ctx = browser.new_context(viewport={"width": 1280, "height": 720})
                page = ctx.new_page()
        else:
            ctx = browser.new_context(viewport={"width": 1280, "height": 720})
            page = ctx.new_page()
    return page

def get_all_pages():
    """Return all pages across all browser contexts."""
    if browser is None:
        get_browser()
    pages = []
    for ctx in browser.contexts:
        pages.extend(ctx.pages)
    return pages

def close_browser():
    global pw, browser, page
    if page is not None:
        try: page.close()
        except Exception: pass
        page = None
    if browser is not None:
        # In attached mode, only disconnect — do not close the user's Chrome.
        try:
            if CDP_TARGET:
                browser.close()  # disconnect, does not shut down remote Chrome
            else:
                browser.close()
        except Exception: pass
        browser = None
    if pw is not None:
        try: pw.stop()
        except Exception: pass
        pw = None

# Track tabs created by the agent (for safety in attached mode).
agent_created_tabs = set()

@app.route("/health")
def health():
    return jsonify({"status": "ok"})

@app.route("/execute", methods=["POST"])
def execute():
    global page
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

        elif action == "list_tabs":
            pages = get_all_pages()
            tabs = []
            for i, pg in enumerate(pages):
                tabs.append({"id": str(i), "url": pg.url, "title": pg.title()})
            result = json.dumps(tabs)

        elif action == "switch_tab":
            tab_id = data.get("tab_id", "0")
            pages = get_all_pages()
            idx = int(tab_id)
            if 0 <= idx < len(pages):
                page = pages[idx]
                page.bring_to_front()
                result = json.dumps({"switched_to": tab_id, "url": page.url, "title": page.title()})
            else:
                return jsonify({"success": False, "error": f"tab_id {tab_id} out of range (0-{len(pages)-1})"})

        elif action == "new_tab":
            url = data.get("url")
            contexts = browser.contexts
            ctx = contexts[0] if contexts else browser.new_context(viewport={"width": 1280, "height": 720})
            new_page = ctx.new_page()
            if url:
                new_page.goto(url, wait_until="domcontentloaded")
            page = new_page
            pages = get_all_pages()
            new_id = str(len(pages) - 1)
            agent_created_tabs.add(new_id)
            result = json.dumps({"tab_id": new_id, "url": new_page.url, "title": new_page.title()})

        elif action == "close_tab":
            tab_id = data.get("tab_id")
            if tab_id is not None:
                # In attached mode, only allow closing tabs the agent created.
                if CDP_TARGET and tab_id not in agent_created_tabs:
                    return jsonify({"success": False, "error": f"cannot close tab {tab_id}: not created by agent"})
                pages = get_all_pages()
                idx = int(tab_id)
                if 0 <= idx < len(pages):
                    target = pages[idx]
                    target.close()
                    agent_created_tabs.discard(tab_id)
                    remaining = get_all_pages()
                    page = remaining[0] if remaining else None
                    result = f"closed tab {tab_id}"
                else:
                    return jsonify({"success": False, "error": f"tab_id {tab_id} out of range"})
            else:
                return jsonify({"success": False, "error": "close_tab requires tab_id"})

        else:
            return jsonify({"success": False, "error": f"unknown action: {action}"})

        return jsonify({"success": True, "result": result})
    except Exception as e:
        return jsonify({"success": False, "error": f"{type(e).__name__}: {e}"})

if __name__ == "__main__":
    port = int(os.environ.get("BRIDGE_PORT", "9223"))
    app.run(host="0.0.0.0", port=port, threaded=False)
"#;

/// Generate the Dockerfile for the browser sidecar.
///
/// The Python bridge script is base64-encoded into a `RUN` command to
/// avoid heredoc parsing issues with Docker's classic builder.
fn browser_dockerfile() -> String {
    let script_b64 = base64::engine::general_purpose::STANDARD.encode(BRIDGE_SCRIPT);
    format!(
        r#"FROM mcr.microsoft.com/playwright/python:v1.49.0
RUN pip install --no-cache-dir flask==3.1.* playwright==1.49.*
RUN echo "{script_b64}" | base64 -d > /opt/browser_bridge.py
USER pwuser
EXPOSE 9223
CMD ["python3", "/opt/browser_bridge.py"]
"#
    )
}

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
    /// When `cdp_target` is `Some`, the sidecar connects to an external Chrome
    /// instance via `connect_over_cdp()` instead of launching its own Chromium.
    /// Pass `None` for standalone mode (headless Chromium inside the container).
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorError`] if any Docker operation fails or the
    /// health check does not pass within the retry budget.
    pub async fn ensure(
        docker: &Docker,
        image: &str,
        workspace_dir: &Path,
        cdp_target: Option<&str>,
    ) -> Result<Self, ExecutorError> {
        let dockerfile = browser_dockerfile();
        super::ensure_image(docker, image, Some(&dockerfile)).await?;

        let workspace_str = workspace_dir.to_str().ok_or_else(|| {
            ExecutorError::Infrastructure("workspace path is not valid UTF-8".to_owned())
        })?;

        ensure_browser_container(docker, image, workspace_str, cdp_target).await?;

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

    /// Base URL for the HTTP bridge API (e.g. `http://127.0.0.1:9223`).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// Ensure the browser container exists and is running.
///
/// Inspects the container; if it exists and is running, returns immediately.
/// If it exists but is stopped, starts it. If it does not exist, creates
/// and starts it. Recreates when port or CDP target configuration has changed.
async fn ensure_browser_container(
    docker: &Docker,
    image: &str,
    workspace_dir: &str,
    cdp_target: Option<&str>,
) -> Result<(), ExecutorError> {
    let inspect = docker
        .inspect_container(CONTAINER_NAME, None::<InspectContainerOptions>)
        .await;

    let needs_start = match inspect {
        Ok(state) => {
            // Check if the existing container has the correct port mapping.
            // If the published port changed (e.g. 9222→9223), we must recreate.
            let port_matches = state
                .host_config
                .as_ref()
                .and_then(|hc| hc.port_bindings.as_ref())
                .and_then(|pb| pb.get(&format!("{BRIDGE_PORT}/tcp")))
                .is_some();

            // Check if the CDP target matches the requested config.
            // An existing standalone container must be recreated for attached mode.
            let existing_cdp =
                state
                    .config
                    .as_ref()
                    .and_then(|c| c.env.as_ref())
                    .and_then(|vars| {
                        vars.iter()
                            .find_map(|v| v.strip_prefix("CDP_TARGET="))
                            .map(str::to_owned)
                    });
            let cdp_matches = match (existing_cdp.as_deref(), cdp_target) {
                (None, None) => true,
                (Some(a), Some(b)) => a == b,
                _ => false,
            };

            if !port_matches || !cdp_matches {
                info!("browser sidecar config mismatch — recreating container");
                let remove_opts = RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                };
                let _ = docker
                    .remove_container(CONTAINER_NAME, Some(remove_opts))
                    .await;
                create_browser_container(docker, image, workspace_dir, cdp_target).await?;
                true
            } else {
                let running = state.state.and_then(|s| s.running).unwrap_or(false);
                !running
            }
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => {
            create_browser_container(docker, image, workspace_dir, cdp_target).await?;
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
///
/// When `cdp_target` is provided, the container receives a `CDP_TARGET` env
/// var that tells the bridge script to connect to an external Chrome instance
/// instead of launching its own Chromium.
async fn create_browser_container(
    docker: &Docker,
    image: &str,
    workspace_dir: &str,
    cdp_target: Option<&str>,
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
        // Ensure host.docker.internal resolves on Linux (standard since Docker 20.10).
        extra_hosts: Some(vec!["host.docker.internal:host-gateway".to_owned()]),
        ..Default::default()
    };

    let mut exposed_ports = HashMap::new();
    exposed_ports.insert(port_key, HashMap::new());

    // Only BRIDGE_PORT and optionally CDP_TARGET are passed.
    // No secrets or config leaked into the browser sidecar.
    let mut env_vars = vec![format!("BRIDGE_PORT={BRIDGE_PORT}")];
    if let Some(target) = cdp_target {
        env_vars.push(format!("CDP_TARGET={target}"));
    }

    let container_config = ContainerConfig {
        image: Some(image.to_owned()),
        labels: Some(labels),
        host_config: Some(host_config),
        exposed_ports: Some(exposed_ports),
        env: Some(env_vars),
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
