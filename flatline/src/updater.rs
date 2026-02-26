//! Auto-update: check GitHub Releases, download, verify, swap, health-watch, rollback.
//!
//! Flatline checks for new releases daily, downloads binaries with SHA256
//! verification, swaps them in place, monitors health, and rolls back on failure.
//! The user stays informed via Telegram at every step.

use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::Timelike;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};
use wintermute::config::RuntimePaths;
use wintermute::heartbeat::health::HealthReport;

use crate::config::{FlatlinePaths, UpdateConfig};
use crate::db::StateDb;
use crate::reporter::Reporter;
use crate::watcher::Watcher;

/// Current package version, set at compile time.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Compilation target triple, set by `build.rs`.
const TARGET: &str = env!("TARGET");

/// Special exit code that signals systemd to restart with the new binary.
pub const EXIT_CODE_SELF_UPDATE: i32 = 10;

/// Interval (seconds) between health polls during the post-update watch.
const HEALTH_POLL_INTERVAL_SECS: u64 = 10;

/// GitHub API base URL.
const GITHUB_API_BASE: &str = "https://api.github.com";

/// Maximum seconds to wait for a migration script to complete.
const MIGRATION_TIMEOUT_SECS: u64 = 120;

// -- Public types --

/// Status of an update through its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    /// Update detected, not yet downloaded.
    Pending,
    /// Binary is being downloaded.
    Downloading,
    /// Update is being applied (binary swap + restart).
    Applying,
    /// Update applied and health checks passed.
    Healthy,
    /// Update failed health checks and was rolled back.
    RolledBack,
    /// Update process failed (download, checksum, etc.).
    Failed,
    /// User chose to skip this version.
    Skipped,
    /// Version is pinned, update not applied.
    Pinned,
}

impl UpdateStatus {
    /// Return the string representation matching the database column.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Downloading => "downloading",
            Self::Applying => "applying",
            Self::Healthy => "healthy",
            Self::RolledBack => "rolled_back",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Pinned => "pinned",
        }
    }
}

/// Information about a GitHub release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseInfo {
    /// Semantic version string (without "v" prefix).
    pub version: String,
    /// Git tag name (e.g. "v0.4.0").
    pub tag_name: String,
    /// Whether this is a pre-release.
    pub prerelease: bool,
    /// Release body / changelog text.
    pub changelog: String,
    /// List of asset download URLs.
    pub assets: Vec<ReleaseAsset>,
}

/// A downloadable asset from a GitHub release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseAsset {
    /// Asset filename.
    pub name: String,
    /// Direct download URL.
    pub browser_download_url: String,
}

/// Holds state for the update lifecycle within a single daemon run.
pub struct Updater {
    config: UpdateConfig,
    paths: FlatlinePaths,
    wm_paths: RuntimePaths,
    http: reqwest::Client,
}

impl Updater {
    /// Create a new updater.
    pub fn new(config: UpdateConfig, paths: FlatlinePaths, wm_paths: RuntimePaths) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(format!("flatline/{VERSION}"))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            config,
            paths,
            wm_paths,
            http,
        }
    }

    /// Check GitHub Releases API for a newer version.
    ///
    /// Returns `None` if already up-to-date, pinned, or updates are disabled.
    ///
    /// # Errors
    ///
    /// Returns an error if the API call fails or the response cannot be parsed.
    pub async fn check_for_update(&self) -> anyhow::Result<Option<ReleaseInfo>> {
        if !self.config.enabled {
            return Ok(None);
        }

        // Pinned version: skip update check.
        if self.config.pinned_version.is_some() {
            debug!("version is pinned, skipping update check");
            return Ok(None);
        }

        let current = parse_version_tag(VERSION)?;

        let url = if self.config.channel == "nightly" {
            format!("{GITHUB_API_BASE}/repos/{}/releases", self.config.repo)
        } else {
            format!(
                "{GITHUB_API_BASE}/repos/{}/releases/latest",
                self.config.repo
            )
        };

        debug!(url = %url, "checking for updates");

        let mut request = self
            .http
            .get(&url)
            .header("Accept", "application/vnd.github+json");

        // Use GITHUB_TOKEN if available for higher rate limits.
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            request = request.header("Authorization", format!("Bearer {token}"));
        }

        let response = request.send().await.context("GitHub API request failed")?;

        if !response.status().is_success() {
            let status = response.status();
            anyhow::bail!("GitHub API returned {status}");
        }

        let release = if self.config.channel == "nightly" {
            // Find the first prerelease in the list.
            let releases: Vec<GitHubRelease> =
                response.json().await.context("failed to parse releases")?;
            releases.into_iter().find(|r| r.prerelease)
        } else {
            let release: GitHubRelease = response
                .json()
                .await
                .context("failed to parse latest release")?;
            Some(release)
        };

        let Some(release) = release else {
            debug!("no matching release found");
            return Ok(None);
        };

        let remote = parse_version_tag(&release.tag_name)?;

        if remote <= current {
            debug!(current = %current, remote = %remote, "already up to date");
            return Ok(None);
        }

        info!(current = %current, remote = %remote, "new version available");

        let assets = release
            .assets
            .into_iter()
            .map(|a| ReleaseAsset {
                name: a.name,
                browser_download_url: a.browser_download_url,
            })
            .collect();

        Ok(Some(ReleaseInfo {
            version: remote.to_string(),
            tag_name: release.tag_name,
            prerelease: release.prerelease,
            changelog: release.body.unwrap_or_default(),
            assets,
        }))
    }

    /// Download the release binaries and checksum file to the pending directory.
    ///
    /// Verifies SHA256 of each downloaded binary against the checksums file.
    /// Returns the list of downloaded binary paths.
    ///
    /// # Errors
    ///
    /// Returns an error if download or checksum verification fails.
    pub async fn download_release(&self, release: &ReleaseInfo) -> anyhow::Result<Vec<PathBuf>> {
        std::fs::create_dir_all(&self.paths.pending_dir).with_context(|| {
            format!(
                "failed to create pending dir {}",
                self.paths.pending_dir.display()
            )
        })?;

        // Download checksums file first.
        let checksums_asset = release
            .assets
            .iter()
            .find(|a| a.name == "checksums-sha256.txt")
            .ok_or_else(|| anyhow::anyhow!("release missing checksums-sha256.txt"))?;

        let checksums_content = self
            .http
            .get(&checksums_asset.browser_download_url)
            .send()
            .await
            .context("failed to download checksums")?
            .text()
            .await
            .context("failed to read checksums body")?;

        let checksums_path = self.paths.pending_dir.join("checksums-sha256.txt");
        tokio::fs::write(&checksums_path, &checksums_content)
            .await
            .context("failed to write checksums file")?;

        // Download each binary.
        let binary_names = [
            format!("wintermute-{}-{TARGET}.tar.gz", release.version),
            format!("flatline-{}-{TARGET}.tar.gz", release.version),
        ];

        let mut downloaded = Vec::new();

        for name in &binary_names {
            validate_asset_name(name)?;

            let asset = release
                .assets
                .iter()
                .find(|a| &a.name == name)
                .ok_or_else(|| anyhow::anyhow!("release missing asset: {name}"))?;

            let dest = self.paths.pending_dir.join(name);
            info!(asset = %name, "downloading release asset");

            let response = self
                .http
                .get(&asset.browser_download_url)
                .send()
                .await
                .with_context(|| format!("failed to download {name}"))?;

            if !response.status().is_success() {
                anyhow::bail!("download of {name} returned {}", response.status());
            }

            let bytes = response
                .bytes()
                .await
                .with_context(|| format!("failed to read body for {name}"))?;

            tokio::fs::write(&dest, &bytes)
                .await
                .with_context(|| format!("failed to write {}", dest.display()))?;

            // Verify SHA256.
            let expected = find_checksum(&checksums_content, name)?;
            let actual = sha256_bytes(&bytes);

            if actual != expected {
                // Clean up the bad download.
                let _ = tokio::fs::remove_file(&dest).await;
                anyhow::bail!("SHA256 mismatch for {name}: expected {expected}, got {actual}");
            }

            info!(asset = %name, sha256 = %actual, "checksum verified");
            downloaded.push(dest);
        }

        Ok(downloaded)
    }

    /// Check whether Wintermute is idle (no active sessions, heartbeat fresh).
    pub fn is_idle(&self, health: &HealthReport) -> bool {
        health.active_sessions == 0
    }

    /// Execute the full update sequence.
    ///
    /// Returns `true` if the update was healthy, `false` if it was rolled back.
    ///
    /// # Errors
    ///
    /// Returns an error if the update process encounters an unrecoverable failure.
    pub async fn apply_update(
        &self,
        release: &ReleaseInfo,
        db_id: i64,
        db: &StateDb,
        reporter: &mut Reporter,
        watcher: &Watcher,
    ) -> anyhow::Result<bool> {
        let now = chrono::Utc::now().to_rfc3339();

        // Step 1: Pull Docker images.
        reporter
            .send_update_progress(&release.version, "pulling Docker images")
            .await
            .ok();

        if let Err(e) = self.pull_docker_images(release).await {
            warn!(error = %e, "docker image pull failed, deferring update");
            reporter
                .send_update_result(VERSION, &release.version, false, Some(&e.to_string()))
                .await
                .ok();
            return Err(e);
        }

        // Step 2: Stop Wintermute.
        reporter
            .send_update_progress(&release.version, "stopping Wintermute")
            .await
            .ok();

        self.stop_wintermute().await?;

        // Step 3: Backup current binaries.
        self.backup_binary("wintermute").await?;
        self.backup_binary("flatline").await?;

        // Step 4: Replace wintermute binary.
        let version = &release.version;
        let wm_archive = format!("wintermute-{version}-{TARGET}.tar.gz");
        self.swap_binary("wintermute", &wm_archive).await?;

        // Step 5: Run migration if present.
        let migration_log = self.run_migration(release).await?;

        // Update DB record to "applying".
        if let Err(e) = db
            .set_update_status(
                db_id,
                UpdateStatus::Applying.as_str(),
                Some(&now),
                None,
                None,
                migration_log.as_deref(),
            )
            .await
        {
            warn!(error = %e, "failed to update DB status to applying");
        }

        // Step 6: Start Wintermute.
        reporter
            .send_update_progress(&release.version, "starting Wintermute")
            .await
            .ok();

        crate::fixer::start_wintermute(&self.wm_paths).await?;

        // Step 7: Health watch.
        reporter
            .send_update_progress(&release.version, "monitoring health")
            .await
            .ok();

        let healthy = self
            .health_watch(watcher, self.config.health_watch_secs)
            .await;

        if healthy {
            info!(version = %release.version, "update healthy");
            Ok(true)
        } else {
            // Step 8b: Rollback.
            warn!(version = %release.version, "update unhealthy, rolling back");
            self.rollback(
                release,
                db_id,
                db,
                reporter,
                "health checks failed after update",
            )
            .await?;
            Ok(false)
        }
    }

    /// Rollback to previous binaries after a failed update.
    ///
    /// # Errors
    ///
    /// Returns an error if rollback fails (critical — requires manual intervention).
    pub async fn rollback(
        &self,
        release: &ReleaseInfo,
        db_id: i64,
        db: &StateDb,
        reporter: &mut Reporter,
        reason: &str,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();

        // Stop broken Wintermute (aggressive: SIGKILL).
        self.force_stop_wintermute().await?;

        // Restore wintermute.prev.
        let wm_prev = self.paths.updates_dir.join("wintermute.prev");
        let wm_bin = resolve_binary_path("wintermute");
        if wm_prev.exists() {
            tokio::fs::copy(&wm_prev, &wm_bin)
                .await
                .context("failed to restore wintermute.prev")?;
            info!("restored wintermute binary from .prev");
        }

        // Start old Wintermute.
        crate::fixer::start_wintermute(&self.wm_paths).await?;

        // Update DB.
        if let Err(e) = db
            .set_update_status(
                db_id,
                UpdateStatus::RolledBack.as_str(),
                None,
                Some(&now),
                Some(reason),
                None,
            )
            .await
        {
            warn!(error = %e, "failed to update DB status to rolled_back");
        }

        // Notify user.
        reporter
            .send_update_result(VERSION, &release.version, false, Some(reason))
            .await
            .ok();

        info!(reason = %reason, "rollback complete");
        Ok(())
    }

    /// Replace Flatline's own binary and exit with code 10.
    ///
    /// On success this function calls `std::process::exit` and does not return.
    ///
    /// # Errors
    ///
    /// Returns an error if the binary swap fails.
    pub async fn self_update(&self, release: &ReleaseInfo) -> anyhow::Result<()> {
        let version = &release.version;
        let fl_archive = format!("flatline-{version}-{TARGET}.tar.gz");
        let pending = self.paths.pending_dir.join(&fl_archive);

        if !pending.exists() {
            anyhow::bail!(
                "flatline archive not found in pending: {}",
                pending.display()
            );
        }

        // Re-verify SHA256 before replacing (defense against on-disk tampering).
        let checksums_path = self.paths.pending_dir.join("checksums-sha256.txt");
        let checksums_content = tokio::fs::read_to_string(&checksums_path)
            .await
            .context("checksums file not found for re-verification")?;
        let expected = find_checksum(&checksums_content, &fl_archive)?;
        let actual = sha256_file(&pending).await?;
        if actual != expected {
            anyhow::bail!(
                "SHA256 re-verification failed for {fl_archive}: expected {expected}, got {actual}"
            );
        }

        let current_exe =
            std::env::current_exe().context("failed to determine current executable path")?;

        // Extract binary from archive and replace current executable.
        let extracted = extract_binary_from_archive(&pending, "flatline")?;
        tokio::fs::copy(&extracted, &current_exe)
            .await
            .context("failed to replace flatline binary")?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&current_exe, perms)
                .context("failed to set execute permission on flatline")?;
        }

        info!(version = %version, "flatline self-update complete, exiting with code {EXIT_CODE_SELF_UPDATE}");
        std::process::exit(EXIT_CODE_SELF_UPDATE);
    }

    // -- Private helpers --

    /// Pull Docker images for the release version.
    async fn pull_docker_images(&self, release: &ReleaseInfo) -> anyhow::Result<()> {
        let tag = &release.tag_name;

        // Validate tag to prevent injection into docker pull arguments.
        anyhow::ensure!(
            tag.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'),
            "release tag contains invalid characters: {tag}"
        );
        anyhow::ensure!(tag.len() <= 128, "release tag too long: {}", tag.len());

        let repo = &self.config.repo;

        // Parse owner from "owner/repo" format.
        let owner = repo
            .split('/')
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid repo format: {repo}"))?;

        let images = [
            format!("ghcr.io/{owner}/wintermute-sandbox:{tag}"),
            format!("ghcr.io/{owner}/wintermute-browser:{tag}"),
        ];

        for image in &images {
            info!(image = %image, "pulling Docker image");
            let img = image.clone();
            let status = tokio::task::spawn_blocking(move || {
                std::process::Command::new("docker")
                    .args(["pull", &img])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .status()
            })
            .await
            .context("docker pull task panicked")?
            .with_context(|| format!("failed to run docker pull {image}"))?;

            if !status.success() {
                anyhow::bail!("docker pull {image} failed with status {status}");
            }
        }

        Ok(())
    }

    /// Stop Wintermute gracefully (SIGTERM + wait).
    async fn stop_wintermute(&self) -> anyhow::Result<()> {
        self.signal_wintermute(&[], "SIGTERM for update", 10).await
    }

    /// Force-stop Wintermute (SIGKILL).
    async fn force_stop_wintermute(&self) -> anyhow::Result<()> {
        self.signal_wintermute(&["-9"], "SIGKILL for rollback", 2)
            .await
    }

    /// Send a signal to Wintermute by reading its PID file.
    async fn signal_wintermute(
        &self,
        signal_args: &[&str],
        label: &str,
        wait_secs: u64,
    ) -> anyhow::Result<()> {
        match tokio::fs::read_to_string(&self.wm_paths.pid_file).await {
            Ok(pid_contents) => {
                let pid_str = pid_contents.trim();
                if pid_str.is_empty() {
                    return Ok(());
                }
                let pid: u32 = pid_str
                    .parse()
                    .with_context(|| format!("invalid PID: {pid_str:?}"))?;

                info!(pid, label, "sending signal to wintermute");
                let mut args: Vec<String> = signal_args.iter().map(|s| (*s).to_owned()).collect();
                args.push(pid.to_string());
                let _ = tokio::task::spawn_blocking(move || {
                    std::process::Command::new("kill")
                        .args(&args)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                })
                .await;

                tokio::time::sleep(tokio::time::Duration::from_secs(wait_secs)).await;
            }
            Err(_) => {
                debug!(label, "no PID file, wintermute may not be running");
            }
        }
        Ok(())
    }

    /// Backup a binary to the updates directory as `{name}.prev`.
    async fn backup_binary(&self, name: &str) -> anyhow::Result<()> {
        let source = resolve_binary_path(name);
        let dest = self.paths.updates_dir.join(format!("{name}.prev"));

        if source.exists() {
            std::fs::create_dir_all(&self.paths.updates_dir).with_context(|| {
                format!(
                    "failed to create updates dir {}",
                    self.paths.updates_dir.display()
                )
            })?;

            tokio::fs::copy(&source, &dest).await.with_context(|| {
                format!(
                    "failed to backup {} to {}",
                    source.display(),
                    dest.display()
                )
            })?;

            info!(
                source = %source.display(),
                dest = %dest.display(),
                "backed up binary"
            );
        }

        Ok(())
    }

    /// Replace a binary with the one extracted from the downloaded archive.
    ///
    /// Re-verifies the archive SHA256 before extraction (TOCTOU defense).
    async fn swap_binary(&self, name: &str, archive_name: &str) -> anyhow::Result<()> {
        let source = self.paths.pending_dir.join(archive_name);
        let dest = resolve_binary_path(name);

        if !source.exists() {
            anyhow::bail!("pending archive not found: {}", source.display());
        }

        // Re-verify SHA256 before extracting (defense against on-disk tampering).
        let checksums_path = self.paths.pending_dir.join("checksums-sha256.txt");
        let checksums_content = tokio::fs::read_to_string(&checksums_path)
            .await
            .context("checksums file not found for re-verification")?;
        let expected = find_checksum(&checksums_content, archive_name)?;
        let actual = sha256_file(&source).await?;
        if actual != expected {
            anyhow::bail!(
                "SHA256 re-verification failed for {archive_name}: expected {expected}, got {actual}"
            );
        }

        // Extract the binary from the archive.
        let extracted = extract_binary_from_archive(&source, name)?;
        tokio::fs::copy(&extracted, &dest).await.with_context(|| {
            format!(
                "failed to swap {} to {}",
                extracted.display(),
                dest.display()
            )
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&dest, perms).context("failed to set execute permission")?;
        }

        info!(
            archive = %archive_name,
            dest = %dest.display(),
            "swapped binary"
        );

        Ok(())
    }

    /// Run an optional migration script from the release assets.
    ///
    /// Validates the script name, verifies SHA256, and applies a timeout.
    /// Returns captured stdout+stderr if a migration script was found and run.
    async fn run_migration(&self, release: &ReleaseInfo) -> anyhow::Result<Option<String>> {
        // Look for a migration script in assets.
        let migration_asset = release
            .assets
            .iter()
            .find(|a| a.name.starts_with("migrate-"));

        let Some(asset) = migration_asset else {
            return Ok(None);
        };

        // Validate asset name against path traversal.
        validate_asset_name(&asset.name)?;

        info!(script = %asset.name, "running migration script");

        let script_path = self.paths.pending_dir.join(&asset.name);

        // Download if not already present.
        if !script_path.exists() {
            let response = self
                .http
                .get(&asset.browser_download_url)
                .send()
                .await
                .context("failed to download migration script")?;

            let bytes = response
                .bytes()
                .await
                .context("failed to read migration script body")?;

            // Verify SHA256 against the checksums file.
            let checksums_path = self.paths.pending_dir.join("checksums-sha256.txt");
            let checksums_content = tokio::fs::read_to_string(&checksums_path)
                .await
                .context("checksums file not found for migration verification")?;
            let expected = find_checksum(&checksums_content, &asset.name)?;
            let actual_hash = sha256_bytes(&bytes);
            if actual_hash != expected {
                anyhow::bail!(
                    "SHA256 mismatch for migration {}: expected {expected}, got {actual_hash}",
                    asset.name
                );
            }

            tokio::fs::write(&script_path, &bytes)
                .await
                .with_context(|| {
                    format!("failed to write migration script {}", script_path.display())
                })?;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&script_path, perms)
                .context("failed to set migration script executable")?;
        }

        let wintermute_root = self.wm_paths.root.to_string_lossy().to_string();
        let script = script_path.to_string_lossy().to_string();
        let timeout_secs = MIGRATION_TIMEOUT_SECS;

        let output = tokio::time::timeout(
            tokio::time::Duration::from_secs(timeout_secs),
            tokio::task::spawn_blocking(move || {
                std::process::Command::new(&script)
                    .env("WINTERMUTE_ROOT", &wintermute_root)
                    .output()
            }),
        )
        .await
        .with_context(|| format!("migration script timed out after {timeout_secs}s"))?
        .context("migration task panicked")?
        .context("failed to execute migration script")?;

        let log = format!(
            "exit: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );

        if !output.status.success() {
            anyhow::bail!("migration script failed:\n{log}");
        }

        info!("migration script completed successfully");
        Ok(Some(log))
    }

    /// Health watch loop: poll health.json for `duration_secs` seconds.
    ///
    /// Returns `true` if health checks pass consistently.
    async fn health_watch(&self, watcher: &Watcher, duration_secs: u64) -> bool {
        let start = tokio::time::Instant::now();
        let duration = tokio::time::Duration::from_secs(duration_secs);
        let poll_interval = tokio::time::Duration::from_secs(HEALTH_POLL_INTERVAL_SECS);

        let mut healthy_count: u64 = 0;
        let mut total_count: u64 = 0;

        // Wait a short initial period for Wintermute to start writing health.json.
        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;

        while start.elapsed() < duration {
            tokio::time::sleep(poll_interval).await;
            total_count = total_count.saturating_add(1);

            match watcher.read_health() {
                Ok(health) => {
                    if health.status == "running" && health.container_healthy {
                        healthy_count = healthy_count.saturating_add(1);
                        debug!(
                            healthy = healthy_count,
                            total = total_count,
                            "health check passed"
                        );
                    } else {
                        debug!(
                            status = %health.status,
                            container = health.container_healthy,
                            "health check: not fully healthy yet"
                        );
                    }
                }
                Err(e) => {
                    debug!(error = %e, "health check: could not read health.json");
                }
            }
        }

        // Require >80% of checks to pass.
        if total_count == 0 {
            return false;
        }

        #[allow(clippy::cast_precision_loss)]
        let ratio = healthy_count as f64 / total_count as f64;
        let passed = ratio > 0.8;

        info!(
            healthy = healthy_count,
            total = total_count,
            ratio = format!("{ratio:.2}"),
            passed,
            "health watch complete"
        );

        passed
    }
}

// -- Free functions --

/// Parse a version tag (e.g. "v0.4.0" or "0.4.0") into a [`semver::Version`].
///
/// # Errors
///
/// Returns an error if the tag is not valid semver.
pub fn parse_version_tag(tag: &str) -> anyhow::Result<semver::Version> {
    let stripped = tag.strip_prefix('v').unwrap_or(tag);
    semver::Version::parse(stripped).with_context(|| format!("invalid semver tag: {tag}"))
}

/// Compute the SHA256 hex digest of a byte slice.
pub fn sha256_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Compute the SHA256 hex digest of a file.
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub async fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let data = tokio::fs::read(path)
        .await
        .with_context(|| format!("failed to read file for hashing: {}", path.display()))?;

    Ok(sha256_bytes(&data))
}

/// Parse a `checksums-sha256.txt` file and find the digest for a given filename.
///
/// Expected format: `{hex_digest}  {filename}\n` (two spaces between digest and name).
///
/// # Errors
///
/// Returns an error if no matching line is found.
pub fn find_checksum(checksums_content: &str, filename: &str) -> anyhow::Result<String> {
    for line in checksums_content.lines() {
        // Format: "abc123def456  some-file.tar.gz"
        let parts: Vec<&str> = line.splitn(2, "  ").collect();
        if parts.len() == 2 && parts[1].trim() == filename {
            return Ok(parts[0].to_owned());
        }
    }
    anyhow::bail!("no checksum found for {filename}")
}

/// Check whether a time-of-day string (HH:MM) matches the current local hour and minute.
///
/// Returns `true` if the current time is within the same hour:minute window.
pub fn is_check_time(check_time: &str, interval_secs: u64) -> bool {
    let parts: Vec<&str> = check_time.split(':').collect();
    if parts.len() != 2 {
        return false;
    }

    let target_hour: u32 = match parts[0].parse() {
        Ok(h) => h,
        Err(_) => return false,
    };
    let target_minute: u32 = match parts[1].parse() {
        Ok(m) => m,
        Err(_) => return false,
    };

    let now = chrono::Local::now();
    let ch = now.hour();
    let cm = now.minute();

    // Check if current time is within the interval window of the target time.
    let target_mins = target_hour.saturating_mul(60).saturating_add(target_minute);
    let current_mins = ch.saturating_mul(60).saturating_add(cm);

    #[allow(clippy::cast_possible_truncation)]
    let window = (interval_secs / 60) as u32;
    let window = window.max(1);

    current_mins >= target_mins && current_mins < target_mins.saturating_add(window)
}

/// Validate that an asset filename does not contain path traversal or suspicious characters.
///
/// # Errors
///
/// Returns an error if the name is invalid.
pub fn validate_asset_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !name.contains('/'),
        "asset name contains path separator '/'"
    );
    anyhow::ensure!(
        !name.contains('\\'),
        "asset name contains path separator '\\'"
    );
    anyhow::ensure!(
        !name.contains(".."),
        "asset name contains path traversal '..'"
    );
    anyhow::ensure!(
        !name.chars().any(|c| c.is_control()),
        "asset name contains control characters"
    );
    anyhow::ensure!(name.len() <= 256, "asset name exceeds 256 characters");
    Ok(())
}

/// Resolve the path to a named binary (wintermute or flatline).
///
/// Checks `./{name}` first, then falls back to bare `{name}` (PATH lookup).
fn resolve_binary_path(name: &str) -> PathBuf {
    let local = PathBuf::from(format!("./{name}"));
    if local.is_file() {
        local
    } else {
        PathBuf::from(name)
    }
}

/// Extract a named binary from a `.tar.gz` archive.
///
/// Searches for an entry whose filename (excluding directories) matches `binary_name`.
/// Returns the path to the extracted file in the same directory as the archive.
///
/// # Errors
///
/// Returns an error if the archive cannot be read, the binary is not found, or extraction fails.
fn extract_binary_from_archive(archive_path: &Path, binary_name: &str) -> anyhow::Result<PathBuf> {
    let parent = archive_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("archive has no parent directory"))?;

    let file = std::fs::File::open(archive_path)
        .with_context(|| format!("failed to open archive {}", archive_path.display()))?;

    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let dest = parent.join(binary_name);

    for entry_result in archive.entries().context("failed to read tar entries")? {
        let mut entry = entry_result.context("failed to read tar entry")?;
        let entry_path = entry.path().context("failed to read entry path")?;

        // Match by filename only (archives may contain directory prefixes).
        let file_name = entry_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if file_name == binary_name {
            let mut out = std::fs::File::create(&dest)
                .with_context(|| format!("failed to create {}", dest.display()))?;
            std::io::copy(&mut entry, &mut out)
                .with_context(|| format!("failed to extract {binary_name} from archive"))?;

            info!(
                binary = binary_name,
                archive = %archive_path.display(),
                "extracted binary from archive"
            );
            return Ok(dest);
        }
    }

    anyhow::bail!(
        "binary '{binary_name}' not found in archive {}",
        archive_path.display()
    )
}

// -- GitHub API response types (private) --

/// GitHub Release API response (subset of fields we care about).
#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    prerelease: bool,
    body: Option<String>,
    assets: Vec<GitHubAsset>,
}

/// GitHub Release Asset from API response.
#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}
