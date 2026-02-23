//! Log tailing and health file monitoring.
//!
//! Watches Wintermute's JSONL logs and `health.json` file on the filesystem.
//! Uses synchronous `std::fs` reads since these are quick local operations.

use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use wintermute::heartbeat::health::HealthReport;

/// A parsed event from Wintermute's structured JSONL logs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEvent {
    /// ISO 8601 timestamp of the event.
    #[serde(default)]
    pub ts: Option<String>,

    /// Log level (info, warn, error).
    #[serde(default)]
    pub level: Option<String>,

    /// Event type (tool_call, budget, llm_call, etc.).
    #[serde(default)]
    pub event: Option<String>,

    /// Tool name, if this is a tool-related event.
    #[serde(default)]
    pub tool: Option<String>,

    /// Execution duration in milliseconds.
    #[serde(default)]
    pub duration_ms: Option<u64>,

    /// Whether the operation succeeded.
    #[serde(default)]
    pub success: Option<bool>,

    /// Error message, if the operation failed.
    #[serde(default)]
    pub error: Option<String>,
}

/// Watches Wintermute's log directory and health file for changes.
///
/// Tracks file position to avoid re-reading old log lines on each poll.
pub struct Watcher {
    log_dir: PathBuf,
    health_path: PathBuf,
    last_offset: u64,
    last_log_file: Option<PathBuf>,
}

impl Watcher {
    /// Create a new watcher pointed at the given log directory and health file.
    pub fn new(log_dir: PathBuf, health_path: PathBuf) -> Self {
        Self {
            log_dir,
            health_path,
            last_offset: 0,
            last_log_file: None,
        }
    }

    /// Poll for new log events since the last call.
    ///
    /// Finds the most recent `.jsonl` file in the log directory, seeks to the
    /// last known offset, reads new lines, and parses each as a `LogEvent`.
    /// Lines that fail to parse are silently skipped.
    ///
    /// # Errors
    ///
    /// Returns an error if the log directory cannot be read or the log file
    /// cannot be opened.
    pub fn poll_logs(&mut self) -> anyhow::Result<Vec<LogEvent>> {
        let latest = match find_latest_jsonl(&self.log_dir)? {
            Some(path) => path,
            None => return Ok(Vec::new()),
        };

        // If we switched to a different log file, reset the offset.
        if self.last_log_file.as_ref() != Some(&latest) {
            self.last_offset = 0;
            self.last_log_file = Some(latest.clone());
        }

        let file = fs::File::open(&latest)
            .with_context(|| format!("failed to open log file {}", latest.display()))?;

        let metadata = file
            .metadata()
            .with_context(|| format!("failed to read metadata for {}", latest.display()))?;
        let file_len = metadata.len();

        // If the file shrank (rotation), reset offset.
        if file_len < self.last_offset {
            self.last_offset = 0;
        }

        // Nothing new to read.
        if file_len == self.last_offset {
            return Ok(Vec::new());
        }

        let mut reader = BufReader::new(file);
        reader
            .seek(SeekFrom::Start(self.last_offset))
            .with_context(|| format!("failed to seek in log file {}", latest.display()))?;

        const MAX_LINE_LEN: usize = 1_048_576; // 1 MB safety limit.

        let mut events = Vec::new();
        let mut line = String::new();

        loop {
            line.clear();
            let bytes_read = reader
                .read_line(&mut line)
                .with_context(|| format!("failed to read line from {}", latest.display()))?;
            if bytes_read == 0 {
                break;
            }

            if line.len() > MAX_LINE_LEN {
                continue;
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Skip lines that fail to parse — they may be partial or non-JSON.
            if let Ok(event) = serde_json::from_str::<LogEvent>(trimmed) {
                events.push(event);
            }
        }

        // Update offset to current position.
        self.last_offset = reader
            .stream_position()
            .context("failed to get stream position")?;

        Ok(events)
    }

    /// Read and parse the current health report from `health.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed.
    pub fn read_health(&self) -> anyhow::Result<HealthReport> {
        let contents = fs::read_to_string(&self.health_path).with_context(|| {
            format!(
                "failed to read health file at {}",
                self.health_path.display()
            )
        })?;
        let report: HealthReport = serde_json::from_str(&contents).with_context(|| {
            format!(
                "failed to parse health file at {}",
                self.health_path.display()
            )
        })?;
        Ok(report)
    }

    /// Check whether `health.json` is stale (last_heartbeat older than threshold).
    ///
    /// # Errors
    ///
    /// Returns an error if the health file cannot be read/parsed or the
    /// timestamp is malformed.
    pub fn is_health_stale(&self, threshold_secs: u64) -> anyhow::Result<bool> {
        let report = self.read_health()?;
        let last_heartbeat = chrono::DateTime::parse_from_rfc3339(&report.last_heartbeat)
            .with_context(|| {
                format!(
                    "failed to parse last_heartbeat timestamp: {}",
                    report.last_heartbeat
                )
            })?;

        let now = chrono::Utc::now();
        let elapsed = now.signed_duration_since(last_heartbeat).num_seconds();

        // If elapsed is negative (clock skew), treat as not stale.
        if elapsed < 0 {
            return Ok(false);
        }

        let elapsed_u64 = u64::try_from(elapsed).context("elapsed seconds exceeds u64 range")?;

        Ok(elapsed_u64 > threshold_secs)
    }
}

/// Find the most recent `.jsonl` file in a directory by modification time.
fn find_latest_jsonl(dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    if !dir.exists() {
        return Ok(None);
    }

    let entries = fs::read_dir(dir)
        .with_context(|| format!("failed to read log directory {}", dir.display()))?;

    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;

    for entry in entries {
        let entry = entry.context("failed to read directory entry")?;
        let path = entry.path();

        let is_jsonl = path.extension().and_then(|ext| ext.to_str()) == Some("jsonl");

        if !is_jsonl {
            continue;
        }

        let modified = entry
            .metadata()
            .with_context(|| format!("failed to read metadata for {}", path.display()))?
            .modified()
            .with_context(|| format!("failed to read mtime for {}", path.display()))?;

        let is_newer = best
            .as_ref()
            .is_none_or(|(_, best_time)| modified > *best_time);

        if is_newer {
            best = Some((path, modified));
        }
    }

    Ok(best.map(|(path, _)| path))
}
