//! Weekly memory digest: consolidates active memories into USER.md.
//!
//! The digest runs as a builtin scheduled task. It reads all active memories,
//! builds a consolidation prompt, archives stale entries, and writes an updated
//! USER.md that the system prompt can load.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use tracing::{info, warn};

use crate::memory::{MemoryEngine, MemoryStatus};

/// Default cutoff (in days) for archiving stale memories.
pub const DEFAULT_STALE_CUTOFF_DAYS: u64 = 90;

/// Result of running the weekly digest.
#[derive(Debug)]
pub struct DigestResult {
    /// The generated USER.md content.
    pub user_md_content: String,
    /// Number of stale memories archived.
    pub archived_count: u64,
    /// Number of contradictions detected in the prompt (informational).
    pub contradictions_found: u64,
}

/// Build a consolidation prompt from the current USER.md and all active memories.
///
/// The returned string is suitable for feeding to an LLM to produce a merged
/// USER.md that incorporates new memories.
pub fn build_consolidation_prompt(current_user_md: &str, memories: &[String]) -> String {
    let mut prompt = String::with_capacity(4096);

    prompt.push_str(
        "You are a memory consolidation assistant. Your job is to merge the agent's \
         active memories into a single, coherent USER.md document.\n\n",
    );

    prompt.push_str("## Rules\n");
    prompt.push_str("- Keep the document concise (under 200 lines)\n");
    prompt.push_str("- Remove duplicates and contradictions (prefer newer information)\n");
    prompt.push_str("- Organize by topic with clear markdown headings\n");
    prompt.push_str("- Preserve factual accuracy — do not invent information\n");
    prompt.push_str("- Output ONLY the markdown content for USER.md, nothing else\n\n");

    prompt.push_str("## Current USER.md\n```\n");
    if current_user_md.is_empty() {
        prompt.push_str("(empty — first digest)\n");
    } else {
        prompt.push_str(current_user_md);
        if !current_user_md.ends_with('\n') {
            prompt.push('\n');
        }
    }
    prompt.push_str("```\n\n");

    prompt.push_str("## Active Memories to Incorporate\n");
    if memories.is_empty() {
        prompt.push_str("(no new memories)\n");
    } else {
        for (i, mem) in memories.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", i.saturating_add(1), mem));
        }
    }
    prompt.push('\n');

    prompt.push_str(
        "Now produce the updated USER.md content, merging the memories above \
         into the existing document. Remove any contradictions, favouring the \
         most recent information.\n",
    );

    prompt
}

/// Archive memories older than `cutoff_days` by setting their status to `Archived`.
///
/// When `prefetched` is provided, those memories are used instead of re-querying.
/// Returns the number of memories archived.
pub async fn archive_stale_memories(
    memory: &Arc<MemoryEngine>,
    cutoff_days: u64,
    prefetched: Option<&[crate::memory::Memory]>,
) -> anyhow::Result<u64> {
    let owned_memories;
    let all_active = match prefetched {
        Some(mems) => mems,
        None => {
            owned_memories = memory
                .search_by_status(MemoryStatus::Active, 10_000)
                .await
                .context("failed to fetch active memories")?;
            &owned_memories
        }
    };

    let days_i64 = i64::try_from(cutoff_days).unwrap_or(90);
    let cutoff = match chrono::Utc::now().checked_sub_signed(chrono::Duration::days(days_i64)) {
        Some(t) => t,
        None => {
            warn!(
                cutoff_days,
                "cutoff date overflow, using current time (archiving nothing)"
            );
            chrono::Utc::now()
        }
    };
    let cutoff_str = cutoff.to_rfc3339();

    let mut archived = 0u64;
    for mem in all_active {
        let is_stale = mem
            .updated_at
            .as_deref()
            .map(|ts| ts < cutoff_str.as_str())
            .unwrap_or(false);

        if is_stale {
            if let Some(id) = mem.id {
                if let Err(e) = memory
                    .update_memory_status(id, MemoryStatus::Archived)
                    .await
                {
                    warn!(id, error = %e, "failed to archive stale memory");
                } else {
                    archived = archived.saturating_add(1);
                }
            }
        }
    }

    info!(archived, cutoff_days, "stale memory archival complete");
    Ok(archived)
}

/// Write USER.md content to disk atomically.
///
/// Writes to a temporary file first, then renames to prevent partial reads.
pub fn write_user_md(content: &str, user_md_path: &Path) -> anyhow::Result<()> {
    let tmp_path = user_md_path.with_extension("md.tmp");
    std::fs::write(&tmp_path, content).with_context(|| {
        format!(
            "failed to write USER.md temp file at {}",
            tmp_path.display()
        )
    })?;
    std::fs::rename(&tmp_path, user_md_path)
        .with_context(|| format!("failed to rename USER.md to {}", user_md_path.display()))?;
    Ok(())
}

/// Load the current USER.md from disk.
///
/// Returns an empty string if the file does not exist.
pub fn load_user_md(user_md_path: &Path) -> String {
    std::fs::read_to_string(user_md_path).unwrap_or_default()
}

/// Run the full digest process (without the LLM step).
///
/// This handles the non-LLM parts of the digest:
/// 1. Load current USER.md
/// 2. Fetch all active memories
/// 3. Archive stale memories
/// 4. Build the consolidation prompt (caller must invoke LLM separately)
///
/// Returns the prompt and digest metadata. The caller should feed the prompt
/// to an LLM and write the result via [`write_user_md`].
pub async fn prepare_digest(
    memory: &Arc<MemoryEngine>,
    user_md_path: &Path,
    cutoff_days: u64,
) -> anyhow::Result<(String, u64)> {
    let current_user_md = load_user_md(user_md_path);

    // Fetch active memories.
    let active_memories = memory
        .search_by_status(MemoryStatus::Active, 10_000)
        .await
        .context("failed to fetch active memories for digest")?;

    let memory_contents: Vec<String> = active_memories.iter().map(|m| m.content.clone()).collect();

    // Archive stale entries (reuses the already-fetched list to avoid a second query).
    let archived_count =
        archive_stale_memories(memory, cutoff_days, Some(&active_memories)).await?;

    // Build the consolidation prompt.
    let prompt = build_consolidation_prompt(&current_user_md, &memory_contents);

    Ok((prompt, archived_count))
}
