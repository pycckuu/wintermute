//! Service management for launchd (macOS) and systemd (Linux).
//!
//! Provides cross-platform service stop/start/install for Wintermute
//! and Flatline services, used by the `flatline update` CLI command.
//! All `std::process::Command` invocations use hardcoded arguments only.

use std::path::{Path, PathBuf};

use anyhow::Context;
use tracing::{debug, info, warn};

// -- Constants: service file names --

/// macOS launchd plist for the Wintermute agent.
const LAUNCHD_AGENT_PLIST: &str = "com.wintermute.agent.plist";

/// macOS launchd plist for the Flatline supervisor.
const LAUNCHD_FLATLINE_PLIST: &str = "com.wintermute.flatline.plist";

/// Linux systemd unit for the Wintermute agent.
const SYSTEMD_AGENT_UNIT: &str = "wintermute.service";

/// Linux systemd unit for the Flatline supervisor.
const SYSTEMD_FLATLINE_UNIT: &str = "flatline.service";

/// Detected service manager on the current platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceManager {
    /// macOS launchd (`~/Library/LaunchAgents/`).
    Launchd,
    /// Linux systemd user units (`~/.config/systemd/user/`).
    Systemd,
}

/// Resolve the macOS LaunchAgents directory (`~/Library/LaunchAgents/`).
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
fn launchd_agents_dir() -> anyhow::Result<PathBuf> {
    let home = directories::BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    Ok(home.home_dir().join("Library/LaunchAgents"))
}

/// Resolve the Linux systemd user units directory (`~/.config/systemd/user/`).
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
fn systemd_user_dir() -> anyhow::Result<PathBuf> {
    let home = directories::BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    Ok(home.home_dir().join(".config/systemd/user"))
}

/// Detect which service manager is active based on installed service files.
///
/// Checks for the presence of Wintermute service files in the platform's
/// standard service directory. Returns `None` if no service files are
/// installed (user runs processes manually).
pub fn detect() -> Option<ServiceManager> {
    if cfg!(target_os = "macos") {
        if let Ok(dir) = launchd_agents_dir() {
            if dir.join(LAUNCHD_AGENT_PLIST).exists() {
                return Some(ServiceManager::Launchd);
            }
        }
    }

    if cfg!(target_os = "linux") {
        if let Ok(dir) = systemd_user_dir() {
            if dir.join(SYSTEMD_AGENT_UNIT).exists() {
                return Some(ServiceManager::Systemd);
            }
        }
    }

    None
}

/// Stop both Wintermute and Flatline services.
///
/// Stops Wintermute first, then Flatline. Tolerates services that are
/// not currently loaded or running (non-zero exit is logged but not fatal).
///
/// # Errors
///
/// Returns an error if a service stop command cannot be spawned.
pub async fn stop_services(manager: ServiceManager) -> anyhow::Result<()> {
    match manager {
        ServiceManager::Launchd => {
            let agents_dir = launchd_agents_dir().context("failed to resolve LaunchAgents dir")?;
            let wm_plist = agents_dir.join(LAUNCHD_AGENT_PLIST);
            let fl_plist = agents_dir.join(LAUNCHD_FLATLINE_PLIST);

            launchctl_unload(&wm_plist).await;
            launchctl_unload(&fl_plist).await;
        }
        ServiceManager::Systemd => {
            systemctl_action("stop", SYSTEMD_AGENT_UNIT).await;
            systemctl_action("stop", SYSTEMD_FLATLINE_UNIT).await;
        }
    }

    Ok(())
}

/// Install (or reinstall) service files from an extracted dist archive.
///
/// For launchd: copies plist files to `~/Library/LaunchAgents/`.
/// For systemd: copies unit files to `~/.config/systemd/user/` and runs
/// `systemctl --user daemon-reload`.
///
/// # Errors
///
/// Returns an error if file copy or daemon-reload fails.
pub async fn install_service_files(manager: ServiceManager, dist_dir: &Path) -> anyhow::Result<()> {
    match manager {
        ServiceManager::Launchd => {
            let source_dir = dist_dir.join("launchd");
            if !source_dir.is_dir() {
                info!("no launchd/ directory in dist archive, skipping service file install");
                return Ok(());
            }

            let dest_dir = launchd_agents_dir().context("failed to resolve LaunchAgents dir")?;
            std::fs::create_dir_all(&dest_dir)
                .with_context(|| format!("failed to create {}", dest_dir.display()))?;

            copy_file(&source_dir.join(LAUNCHD_AGENT_PLIST), &dest_dir)?;
            copy_file(&source_dir.join(LAUNCHD_FLATLINE_PLIST), &dest_dir)?;
        }
        ServiceManager::Systemd => {
            let source_dir = dist_dir.join("systemd");
            if !source_dir.is_dir() {
                info!("no systemd/ directory in dist archive, skipping service file install");
                return Ok(());
            }

            let dest_dir = systemd_user_dir().context("failed to resolve systemd user dir")?;
            std::fs::create_dir_all(&dest_dir)
                .with_context(|| format!("failed to create {}", dest_dir.display()))?;

            copy_file(&source_dir.join(SYSTEMD_AGENT_UNIT), &dest_dir)?;
            copy_file(&source_dir.join(SYSTEMD_FLATLINE_UNIT), &dest_dir)?;

            // Reload systemd so it picks up the new unit files.
            systemctl_daemon_reload().await?;
        }
    }

    Ok(())
}

/// Start both Flatline and Wintermute services.
///
/// Starts Flatline first (supervisor), then Wintermute.
///
/// # Errors
///
/// Returns an error if a service start command fails.
pub async fn start_services(manager: ServiceManager) -> anyhow::Result<()> {
    match manager {
        ServiceManager::Launchd => {
            let agents_dir = launchd_agents_dir().context("failed to resolve LaunchAgents dir")?;
            let fl_plist = agents_dir.join(LAUNCHD_FLATLINE_PLIST);
            let wm_plist = agents_dir.join(LAUNCHD_AGENT_PLIST);

            launchctl_load(&fl_plist).await?;
            launchctl_load(&wm_plist).await?;
        }
        ServiceManager::Systemd => {
            systemctl_start(SYSTEMD_FLATLINE_UNIT).await?;
            systemctl_start(SYSTEMD_AGENT_UNIT).await?;
        }
    }

    Ok(())
}

// -- Private helpers --

/// Copy a single file into a destination directory.
fn copy_file(source: &Path, dest_dir: &Path) -> anyhow::Result<()> {
    let file_name = source
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("source path has no filename: {}", source.display()))?;

    let dest = dest_dir.join(file_name);

    if source.exists() {
        std::fs::copy(source, &dest).with_context(|| {
            format!("failed to copy {} to {}", source.display(), dest.display())
        })?;
        info!(
            source = %source.display(),
            dest = %dest.display(),
            "installed service file"
        );
    } else {
        debug!(path = %source.display(), "service file not found in dist, skipping");
    }

    Ok(())
}

/// Run `launchctl unload <plist>`. Tolerates failure (service may not be loaded).
async fn launchctl_unload(plist_path: &Path) {
    let path = plist_path.to_string_lossy().to_string();
    info!(plist = %path, "unloading launchd service");

    let result = tokio::task::spawn_blocking(move || {
        std::process::Command::new("launchctl")
            .args(["unload", &path])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    })
    .await;

    match result {
        Ok(Ok(status)) if status.success() => {
            info!("launchd service unloaded");
        }
        Ok(Ok(status)) => {
            debug!(exit_code = ?status.code(), "launchctl unload returned non-zero (service may not have been loaded)");
        }
        Ok(Err(e)) => {
            warn!(error = %e, "failed to run launchctl unload");
        }
        Err(e) => {
            warn!(error = %e, "launchctl unload task panicked");
        }
    }
}

/// Run `launchctl load <plist>`.
///
/// # Errors
///
/// Returns an error if the command fails.
async fn launchctl_load(plist_path: &Path) -> anyhow::Result<()> {
    let path = plist_path.to_string_lossy().to_string();
    info!(plist = %path, "loading launchd service");

    let status = tokio::task::spawn_blocking(move || {
        std::process::Command::new("launchctl")
            .args(["load", &path])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()
    })
    .await
    .context("launchctl load task panicked")?
    .context("failed to run launchctl load")?;

    if !status.success() {
        anyhow::bail!("launchctl load failed with exit code {:?}", status.code());
    }

    Ok(())
}

/// Run `systemctl --user <action> <unit>`. Tolerates failure for stop actions.
async fn systemctl_action(action: &str, unit: &str) {
    let action_owned = action.to_owned();
    let unit_owned = unit.to_owned();
    info!(action = %action, unit = %unit, "running systemctl");

    let result = tokio::task::spawn_blocking(move || {
        std::process::Command::new("systemctl")
            .args(["--user", &action_owned, &unit_owned])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    })
    .await;

    match result {
        Ok(Ok(status)) if status.success() => {
            info!(action = %action, unit = %unit, "systemctl command succeeded");
        }
        Ok(Ok(status)) => {
            debug!(
                action = %action,
                unit = %unit,
                exit_code = ?status.code(),
                "systemctl command returned non-zero (service may not have been running)"
            );
        }
        Ok(Err(e)) => {
            warn!(error = %e, action = %action, unit = %unit, "failed to run systemctl");
        }
        Err(e) => {
            warn!(error = %e, action = %action, unit = %unit, "systemctl task panicked");
        }
    }
}

/// Run `systemctl --user start <unit>`.
///
/// # Errors
///
/// Returns an error if the command fails.
async fn systemctl_start(unit: &str) -> anyhow::Result<()> {
    let unit_owned = unit.to_owned();
    info!(unit = %unit, "starting systemd service");

    let status = tokio::task::spawn_blocking(move || {
        std::process::Command::new("systemctl")
            .args(["--user", "start", &unit_owned])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()
    })
    .await
    .context("systemctl start task panicked")?
    .context("failed to run systemctl start")?;

    if !status.success() {
        anyhow::bail!(
            "systemctl --user start {} failed with exit code {:?}",
            unit,
            status.code()
        );
    }

    Ok(())
}

/// Run `systemctl --user daemon-reload`.
///
/// # Errors
///
/// Returns an error if the command fails.
async fn systemctl_daemon_reload() -> anyhow::Result<()> {
    info!("reloading systemd daemon configuration");

    let status = tokio::task::spawn_blocking(|| {
        std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()
    })
    .await
    .context("systemctl daemon-reload task panicked")?
    .context("failed to run systemctl daemon-reload")?;

    if !status.success() {
        anyhow::bail!(
            "systemctl --user daemon-reload failed with exit code {:?}",
            status.code()
        );
    }

    Ok(())
}
