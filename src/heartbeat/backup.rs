//! Automated backup: directory copy for scripts, VACUUM INTO for memory.
//!
//! Uses pure Rust file operations and SQL commands only — no subprocess
//! invocation — preserving security invariant #1 (no host executor).
//! The `.git/` directory inside scripts is preserved, so full git history
//! is available in backups.

use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::Utc;
use sqlx::SqlitePool;
use tracing::{debug, info, warn};

/// Result of a backup operation.
#[derive(Debug)]
pub struct BackupResult {
    /// Path to the backup directory.
    pub backup_dir: PathBuf,
    /// Whether scripts were copied.
    pub scripts_copied: bool,
    /// Whether memory database was backed up.
    pub memory_copied: bool,
    /// Total size of the backup in bytes.
    pub total_size_bytes: u64,
}

/// Create a backup of scripts and memory database.
///
/// - Scripts: recursive directory copy (preserves `.git/` for history).
/// - Memory: `VACUUM INTO` via sqlx (consistent snapshot, no subprocess).
///
/// Backups are stored in timestamped directories under `backups_dir`.
///
/// # Errors
///
/// Returns an error if directory creation or file operations fail.
pub async fn create_backup(
    scripts_dir: &Path,
    memory_pool: &SqlitePool,
    backups_dir: &Path,
) -> anyhow::Result<BackupResult> {
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let backup_dir = backups_dir.join(&timestamp);

    tokio::fs::create_dir_all(&backup_dir)
        .await
        .context("failed to create backup directory")?;

    info!(dir = %backup_dir.display(), "creating backup");

    // Backup scripts directory.
    let scripts_copied = if scripts_dir.exists() {
        let scripts_dst = backup_dir.join("scripts");
        match copy_dir_recursive(scripts_dir, &scripts_dst).await {
            Ok(size) => {
                debug!(size_bytes = size, "scripts backup complete");
                true
            }
            Err(e) => {
                warn!(error = %e, "scripts backup failed");
                false
            }
        }
    } else {
        debug!("scripts directory does not exist, skipping");
        false
    };

    // Backup memory database using VACUUM INTO (consistent snapshot).
    let memory_dst = backup_dir.join("memory.db");
    let memory_copied = match vacuum_into(memory_pool, &memory_dst).await {
        Ok(()) => {
            debug!("memory backup complete");
            true
        }
        Err(e) => {
            warn!(error = %e, "memory VACUUM INTO failed, attempting file copy");
            false
        }
    };

    // Calculate total size.
    let total_size_bytes = dir_size(&backup_dir).await.unwrap_or(0);

    info!(
        dir = %backup_dir.display(),
        scripts = scripts_copied,
        memory = memory_copied,
        size_bytes = total_size_bytes,
        "backup complete"
    );

    Ok(BackupResult {
        backup_dir,
        scripts_copied,
        memory_copied,
        total_size_bytes,
    })
}

/// Create a consistent SQLite snapshot using VACUUM INTO.
///
/// This is a pure SQL operation — no subprocess needed.
async fn vacuum_into(pool: &SqlitePool, destination: &Path) -> anyhow::Result<()> {
    let dest_str = destination
        .to_str()
        .context("backup path is not valid UTF-8")?;

    // Reject paths containing characters that could interfere with SQL parsing.
    // VACUUM INTO does not support parameterized queries, so we must
    // validate the path before interpolation. The path is always internally
    // generated (backups_dir + timestamp), never user-controlled.
    anyhow::ensure!(
        dest_str
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | ' ')),
        "backup path contains disallowed characters"
    );

    // VACUUM INTO creates a new, defragmented copy of the database.
    let query = format!("VACUUM INTO '{dest_str}'");
    sqlx::raw_sql(&query)
        .execute(pool)
        .await
        .context("VACUUM INTO failed")?;

    Ok(())
}

/// Recursively copy a directory tree.
///
/// Returns the total bytes copied. Uses `spawn_blocking` for filesystem I/O
/// to avoid blocking the Tokio runtime.
async fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<u64> {
    let src = src.to_owned();
    let dst = dst.to_owned();

    tokio::task::spawn_blocking(move || copy_dir_recursive_sync(&src, &dst))
        .await
        .context("copy task panicked")?
}

/// Synchronous recursive directory copy.
fn copy_dir_recursive_sync(src: &Path, dst: &Path) -> anyhow::Result<u64> {
    std::fs::create_dir_all(dst)
        .with_context(|| format!("failed to create directory {}", dst.display()))?;

    let mut total_bytes = 0u64;

    for entry in std::fs::read_dir(src)
        .with_context(|| format!("failed to read directory {}", src.display()))?
    {
        let entry = entry.context("failed to read directory entry")?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type().context("failed to get file type")?;

        if file_type.is_dir() {
            total_bytes =
                total_bytes.saturating_add(copy_dir_recursive_sync(&src_path, &dst_path)?);
        } else if file_type.is_file() {
            let bytes = std::fs::copy(&src_path, &dst_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
            total_bytes = total_bytes.saturating_add(bytes);
        }
        // Skip symlinks and other special file types.
    }

    Ok(total_bytes)
}

/// Calculate total size of a directory recursively.
async fn dir_size(path: &Path) -> anyhow::Result<u64> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || dir_size_sync(&path))
        .await
        .context("dir size task panicked")?
}

/// Synchronous directory size calculation.
fn dir_size_sync(path: &Path) -> anyhow::Result<u64> {
    let mut total = 0u64;

    if path.is_file() {
        return Ok(std::fs::metadata(path)?.len());
    }

    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let p = entry.path();
            if p.is_dir() {
                total = total.saturating_add(dir_size_sync(&p)?);
            } else if p.is_file() {
                total = total.saturating_add(std::fs::metadata(&p)?.len());
            }
        }
    }

    Ok(total)
}
