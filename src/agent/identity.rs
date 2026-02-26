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
use crate::tools::browser::BrowserMode;

/// Snapshot of runtime state used to render the identity document.
#[derive(Debug, Clone)]
pub struct IdentitySnapshot {
    /// Active LLM model identifier.
    pub model_id: String,
    /// Executor type (Docker or Direct).
    pub executor_kind: ExecutorKind,
    /// Number of core tools.
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
    /// Agent display name from personality config.
    pub agent_name: String,
    /// Current browser mode (attached to Chrome, standalone sidecar, or unavailable).
    pub browser_mode: BrowserMode,
}

/// Render the identity document from a runtime snapshot.
///
/// Produces a markdown document covering architecture, tools, memory,
/// budget, privacy boundary, and self-modification guidance.
pub fn render_identity(snap: &IdentitySnapshot) -> String {
    let mut doc = String::with_capacity(6144);

    // Header
    let _ = writeln!(doc, "# {}\n", snap.agent_name);
    let _ = writeln!(
        doc,
        "You are {}, a self-coding AI agent.\n",
        snap.agent_name
    );

    // Architecture
    doc.push_str("## Your Architecture\n");
    let _ = writeln!(doc, "- Model: {}", snap.model_id);

    let executor_label = match snap.executor_kind {
        ExecutorKind::Docker => "Docker sandbox (outbound via egress proxy)",
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

    // Topology (Docker mode only)
    if snap.executor_kind == ExecutorKind::Docker {
        doc.push_str(
            "\
## Topology
```
HOST → browser (CDP or sidecar) + egress-proxy (Squid) → sandbox → service containers
```
- `browser` controls your Chrome via CDP (or Docker sidecar fallback).
- The sandbox runs your code. Service containers (databases, etc.) are managed via `docker_manage`.
- All outbound traffic from the sandbox goes through the egress proxy.

",
        );
    }

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
    doc.push_str("- Core tools: execute_command, web_fetch (+ save_to for file downloads), web_request, browser, memory_search, memory_save, send_telegram, create_tool, docker_manage\n");
    doc.push('\n');

    // Browser
    doc.push_str("## Browser\n");
    match &snap.browser_mode {
        BrowserMode::Attached { port } => {
            let _ = writeln!(
                doc,
                "Connected to your Chrome on port {port}. I can see your tabs and use your \
                 logins. When I fill forms, I won't submit — I'll let you review first. I \
                 won't type passwords. If I need you to log in somewhere, I'll ask."
            );
        }
        BrowserMode::Standalone { .. } => {
            doc.push_str(
                "Using a standalone browser (no access to your logins). Good for research \
                 and scraping. For tasks needing your accounts, run Chrome with \
                 --remote-debugging-port=9222 and I'll connect.\n",
            );
        }
        BrowserMode::None => {
            doc.push_str("No browser available.\n");
        }
    }
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
    match snap.executor_kind {
        ExecutorKind::Docker => {
            doc.push_str(
                "- Your sandbox has network, but ALL traffic goes through an egress proxy.\n",
            );
            doc.push_str("- Only domains in the allowlist (config.toml) are reachable.\n");
        }
        ExecutorKind::Direct => {
            doc.push_str("- Running in direct mode without network isolation. Be careful with outbound requests.\n");
        }
    }
    doc.push_str(
        "\
- POST to unknown domains requires user approval.
- Everything in /scripts/ is git-versioned.

",
    );

    // Self-modification: what you can modify
    doc.push_str(
        "\
## What You Can Modify About Yourself
You can evolve. This is by design.

**Your name and personality (agent.toml → [personality]):**
You can rename yourself. You can rewrite your own soul. If the user asks you to be more concise,
more opinionated, funnier, more formal — update your soul. If you notice
your communication style doesn't match what the user wants, propose a
change. The soul is loaded fresh every conversation, so changes take
effect immediately.

**Your tools (/scripts/):** create_tool makes new capabilities permanent.

**Your memory:** memory_save accumulates knowledge over time.

**Your scheduled tasks (agent.toml → [[scheduled_tasks]]):** add, modify,
or remove automated tasks.

**Your services (agent.toml → [[services]]):** spin up or tear down
Docker services you depend on.

",
    );

    // Self-modification: what you cannot modify
    doc.push_str(
        "\
## What You CANNOT Modify
**config.toml** — security policy, credentials, domain allowlist, budget
limits. These are the user's constraints on you. Respect them.

**IDENTITY.md** — your self-knowledge document. Generated by the system,
not by you. You read it, you don't write it.

**USER.md** — the user's profile. Updated by the weekly digest, not by
you directly. You contribute to it by saving memories.

**Your core binary** — the Rust code that runs you. You extend yourself
through tools and config, not by recompiling.

",
    );

    // Self-modification protocol
    doc.push_str(
        "\
## Self-Modification Protocol
When modifying your own soul or config:
1. Tell the user what you want to change and why
2. Show the before/after
3. Wait for approval (this is a personality change, not a tool call)
4. Apply via execute_command editing agent.toml
5. Git commit: \"evolve: {what changed}\"
6. The change takes effect on the next conversation

",
    );

    // What you can help set up
    doc.push_str(
        "\
## What You Can Help Set Up
- Spin up services: docker_manage to run Ollama, databases, Redis, etc.
- Enable vector search: docker_manage to run Ollama + pull embedding model
- Add new domains: request approval, they get added to the allowlist
- Install packages: pip install directly in the sandbox
- Create scheduled tasks: edit agent.toml to add cron-triggered tools
- Configure model routing: explain per-role/per-skill model options

",
    );

    // Handling non-text messages
    doc.push_str(
        "\
## Handling Non-Text Messages
When you receive a voice message, photo, or document you can't process:
- Tell the user what you received and where the file is
- Offer to build a tool to handle it using create_tool
- For voice: transcribe via whisper (Ollama model, pip package, or API)
- For images: describe via multimodal model or OCR via tesseract
- For documents: extract text via Python libraries (pypdf, docx, etc.)
Once you create the tool, you'll handle that media type automatically.
",
    );

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
