//! System Identity Document (SID) generator.
//!
//! Produces `~/.wintermute/IDENTITY.md`, a markdown document loaded into
//! every conversation's system prompt so the agent has accurate self-knowledge
//! about its architecture, tools, memory, and current runtime state.
//!
//! The SID is **generated**, not hand-written. The heartbeat regenerates it
//! periodically so it reflects current state.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use anyhow::Context;

use crate::executor::ExecutorKind;

/// Snapshot of runtime state used to render the identity document.
#[derive(Debug, Clone)]
pub struct IdentitySnapshot {
    /// Active LLM model identifier.
    pub model_id: String,
    /// Executor type (Docker or Direct).
    pub executor_kind: ExecutorKind,
    /// Whether the executor has network isolation.
    pub has_network_isolation: bool,
    /// Number of core tools (always 8).
    pub core_tool_count: usize,
    /// Number of dynamic (agent-created) tools.
    pub dynamic_tool_count: usize,
    /// Total active memories.
    pub active_memory_count: u64,
    /// Pending memories awaiting promotion.
    pub pending_memory_count: u64,
    /// Whether vector search is configured.
    pub has_vector_search: bool,
    /// Maximum tokens per session.
    pub session_budget_limit: u64,
    /// Maximum tokens per day.
    pub daily_budget_limit: u64,
    /// Process uptime.
    pub uptime: Duration,
}

/// Render the identity document from a runtime snapshot.
pub fn render_identity(snap: &IdentitySnapshot) -> String {
    let mut doc = String::with_capacity(2048);

    // Header
    doc.push_str("# Wintermute\n\n");
    doc.push_str("You are Wintermute, a self-coding AI agent.\n\n");

    // Architecture
    doc.push_str("## Your Architecture\n");
    let _ = writeln!(doc, "- Model: {}", snap.model_id);

    let executor_label = match snap.executor_kind {
        ExecutorKind::Docker => "Docker sandbox (network-isolated container)",
        ExecutorKind::Direct => "Direct mode (host-local, no container isolation)",
    };
    let _ = writeln!(doc, "- Executor: {executor_label}");

    let search_mode = if snap.has_vector_search {
        "SQLite + FTS5 + vector search"
    } else {
        "SQLite + FTS5 (keyword search only)"
    };
    let _ = writeln!(doc, "- Memory: {search_mode}");

    let _ = writeln!(doc, "- Uptime: {}", format_uptime(snap.uptime));
    doc.push('\n');

    // Tools
    doc.push_str("## Your Tools\n");
    let _ = writeln!(
        doc,
        "- {} core tools (always available)",
        snap.core_tool_count
    );
    let _ = writeln!(
        doc,
        "- {} custom tools (agent-created)",
        snap.dynamic_tool_count
    );
    doc.push('\n');

    // Memory
    doc.push_str("## Your Memory\n");
    let _ = writeln!(doc, "- {} active memories", snap.active_memory_count);
    let _ = writeln!(
        doc,
        "- {} pending memories awaiting promotion",
        snap.pending_memory_count
    );
    if !snap.has_vector_search {
        doc.push_str(
            "- Vector search not configured. You can enable it by configuring an embedding model.\n",
        );
    }
    doc.push('\n');

    // Budget
    doc.push_str("## Budget\n");
    let _ = writeln!(doc, "- Session limit: {} tokens", snap.session_budget_limit);
    let _ = writeln!(doc, "- Daily limit: {} tokens", snap.daily_budget_limit);
    doc.push('\n');

    // Privacy boundary
    doc.push_str("## Privacy Boundary\n");
    if snap.has_network_isolation {
        doc.push_str(
            "- Your sandbox has NO network access. Use web_fetch/web_request for internet.\n",
        );
    } else {
        doc.push_str("- Running in direct mode without network isolation. Be careful with outbound requests.\n");
    }
    doc.push_str("- POST to unknown domains requires user approval.\n");
    doc.push_str("- Everything in /scripts/ is git-versioned.\n");

    doc
}

/// Write the identity document to disk atomically.
///
/// Writes to a `.tmp` file first, then renames to avoid partial reads.
///
/// # Errors
///
/// Returns an error if the write or rename fails.
pub fn write_identity_file(content: &str, path: &Path) -> anyhow::Result<()> {
    let tmp_path = path.with_extension("md.tmp");
    std::fs::write(&tmp_path, content).with_context(|| {
        format!(
            "failed to write identity temp file at {}",
            tmp_path.display()
        )
    })?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("failed to rename identity file to {}", path.display()))?;
    Ok(())
}

/// Load the identity document from disk.
///
/// Returns `None` if the file does not exist or cannot be read.
pub fn load_identity(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Format a duration as a human-readable string like "2h 15m" or "45m 30s".
pub fn format_uptime(d: Duration) -> String {
    let total_secs = d.as_secs();
    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let minutes = (total_secs % 3600) / 60;

    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        let secs = total_secs % 60;
        format!("{minutes}m {secs}s")
    }
}
