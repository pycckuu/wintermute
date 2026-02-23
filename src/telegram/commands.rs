//! Telegram slash command handlers.
//!
//! Each function handles a specific command and returns an HTML-formatted
//! response string. All output uses HTML parse mode per project convention.

use crate::executor::Executor;
use crate::memory::MemoryEngine;
use crate::telegram::ui::{escape_html, format_budget};
use crate::tools::registry::DynamicToolRegistry;

/// List all available commands.
pub fn handle_help() -> String {
    [
        "<b>Available commands:</b>",
        "",
        "/help — show this message",
        "/status — executor health, memory stats, active sessions",
        "/budget — token budget usage",
        "/memory — search recent memories",
        "/memory_pending — show pending observer memories",
        "/memory_undo — undo last observer promotion",
        "/tools — list dynamic tools",
        "/tools &lt;name&gt; — show detail for a specific tool",
        "/sandbox — container/executor status",
        "/backup — trigger a backup",
    ]
    .join("\n")
}

/// Show system status: executor health, memory count, active sessions.
pub async fn handle_status(
    executor: &dyn Executor,
    memory: &MemoryEngine,
    session_count: usize,
) -> String {
    let health = match executor.health_check().await {
        Ok(h) => format!("{h:?}"),
        Err(e) => format!("error: {e}"),
    };

    let memory_count = match memory.search("*", 1000).await {
        Ok(results) => results.len(),
        Err(_) => 0,
    };

    format!(
        "<b>Status</b>\n\
         Executor: {executor_health}\n\
         Memories: ~{memory_count}\n\
         Active sessions: {session_count}",
        executor_health = escape_html(&health),
    )
}

/// Format budget usage from pre-fetched values.
pub fn handle_budget(
    session_used: u64,
    daily_used: u64,
    session_limit: u64,
    daily_limit: u64,
) -> String {
    format_budget(session_used, daily_used, session_limit, daily_limit)
}

/// Search for recent memories and return a summary.
pub async fn handle_memory(memory: &MemoryEngine) -> String {
    match memory.search("recent", 5).await {
        Ok(results) if results.is_empty() => "No memories found.".to_owned(),
        Ok(results) => {
            let mut lines = vec!["<b>Recent memories:</b>".to_owned()];
            for mem in &results {
                let kind = mem.kind.as_str();
                let content = escape_html(&mem.content);
                // Truncate long content for display
                let display = if content.len() > 120 {
                    let truncated: String = content.chars().take(120).collect();
                    format!("{truncated}...")
                } else {
                    content
                };
                lines.push(format!("  [{kind}] {display}"));
            }
            lines.join("\n")
        }
        Err(e) => format!("Memory search error: {}", escape_html(&e.to_string())),
    }
}

/// Show pending observer memories.
pub async fn handle_memory_pending(memory: &MemoryEngine) -> String {
    use crate::memory::MemoryStatus;

    let pending = match memory.search_by_status(MemoryStatus::Pending, 20).await {
        Ok(p) => p,
        Err(e) => return format!("Error: {}", escape_html(&e.to_string())),
    };

    if pending.is_empty() {
        return "No pending observer memories.".to_owned();
    }

    let mut lines = vec![format!("<b>Pending memories ({}):</b>", pending.len())];
    for mem in &pending {
        let kind = mem.kind.as_str();
        let content = escape_html(&mem.content);
        let display = if content.len() > 120 {
            let truncated: String = content.chars().take(120).collect();
            format!("{truncated}...")
        } else {
            content
        };
        lines.push(format!("  [{kind}] {display}"));
    }
    lines.join("\n")
}

/// Undo the last batch of observer-promoted memories.
pub async fn handle_memory_undo(memory: &MemoryEngine) -> String {
    match crate::observer::staging::undo_last_promotion(memory).await {
        Ok(0) => "No observer-promoted memories to undo.".to_owned(),
        Ok(count) => format!("Reverted {count} observer-promoted memories to archived."),
        Err(e) => format!("Undo failed: {}", escape_html(&e.to_string())),
    }
}

/// List all dynamic tools with descriptions.
pub fn handle_tools(registry: &DynamicToolRegistry) -> String {
    let defs = registry.all_definitions();
    if defs.is_empty() {
        return "No dynamic tools registered.".to_owned();
    }

    let mut lines = vec![format!("<b>Dynamic tools ({}):</b>", defs.len())];
    for def in &defs {
        let name = escape_html(&def.name);
        let desc = escape_html(&def.description);
        lines.push(format!("  <code>{name}</code> — {desc}"));
    }
    lines.join("\n")
}

/// Show detail for a specific dynamic tool.
pub fn handle_tools_detail(registry: &DynamicToolRegistry, name: &str) -> String {
    match registry.get(name) {
        Some(schema) => {
            let escaped_name = escape_html(&schema.name);
            let escaped_desc = escape_html(&schema.description);
            let params = serde_json::to_string_pretty(&schema.parameters)
                .unwrap_or_else(|_| schema.parameters.to_string());
            let escaped_params = escape_html(&params);
            format!(
                "<b>Tool:</b> <code>{escaped_name}</code>\n\
                 <b>Description:</b> {escaped_desc}\n\
                 <b>Timeout:</b> {timeout}s\n\
                 <b>Parameters:</b>\n<pre>{escaped_params}</pre>",
                timeout = schema.timeout_secs,
            )
        }
        None => format!("Tool <code>{}</code> not found.", escape_html(name)),
    }
}

/// Show container/executor status.
pub async fn handle_sandbox(executor: &dyn Executor) -> String {
    let health = match executor.health_check().await {
        Ok(h) => format!("{h:?}"),
        Err(e) => format!("error: {e}"),
    };

    format!(
        "<b>Sandbox</b>\n\
         Kind: {:?}\n\
         Network isolation: {}\n\
         Health: {}",
        executor.kind(),
        executor.has_network_isolation(),
        escape_html(&health),
    )
}

/// Trigger an immediate backup.
pub async fn handle_backup_trigger(
    scripts_dir: &std::path::Path,
    memory_pool: &sqlx::SqlitePool,
    backups_dir: &std::path::Path,
) -> String {
    match crate::heartbeat::backup::create_backup(scripts_dir, memory_pool, backups_dir).await {
        Ok(result) => {
            let size_kb = result.total_size_bytes / 1024;
            format!(
                "<b>Backup created</b>\nPath: <code>{}</code>\nScripts: {}\nMemory: {}\nSize: {} KB",
                escape_html(&result.backup_dir.display().to_string()),
                if result.scripts_copied { "yes" } else { "no" },
                if result.memory_copied { "yes" } else { "no" },
                size_kb,
            )
        }
        Err(e) => format!("Backup failed: {}", escape_html(&e.to_string())),
    }
}
