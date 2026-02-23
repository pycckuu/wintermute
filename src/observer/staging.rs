//! Staged promotion of observer extractions to active memory.
//!
//! Extractions are saved as pending memories. Promotion to active status
//! depends on the configured [`PromotionMode`]: automatic after a threshold
//! of consistent extractions, suggested to the user, or disabled.

use anyhow::Context;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::agent::TelegramOutbound;
use crate::config::{LearningConfig, PromotionMode};
use crate::memory::{Memory, MemoryEngine, MemoryKind, MemorySource, MemoryStatus};
use crate::telegram::ui::escape_html;

use super::extractor::{Extraction, ExtractionKind};

/// Result of staging a batch of extractions.
#[derive(Debug)]
pub struct StagingResult {
    /// Number of new pending memories created.
    pub staged: usize,
    /// Number of extractions that duplicated existing memories.
    pub duplicates: usize,
    /// Number of extractions that contradicted existing memories.
    pub contradictions: usize,
}

/// Result of a promotion check.
#[derive(Debug)]
pub struct PromotionResult {
    /// Number of memories promoted to active.
    pub promoted: usize,
    /// Number of memories suggested to user (suggest mode).
    pub suggested: usize,
}

/// Stage extractions as pending memories, checking for duplicates and contradictions.
///
/// Each extraction is checked against existing active memories using FTS5 search.
/// Duplicates are skipped. Contradictions are flagged in metadata.
///
/// # Errors
///
/// Returns an error if memory operations fail.
pub async fn stage_extractions(
    extractions: &[Extraction],
    memory: &MemoryEngine,
    session_id: &str,
) -> anyhow::Result<StagingResult> {
    let mut staged: usize = 0;
    let mut duplicates: usize = 0;
    let mut contradictions: usize = 0;

    for extraction in extractions {
        let kind = match extraction.kind {
            ExtractionKind::Fact | ExtractionKind::Preference => MemoryKind::Fact,
            ExtractionKind::Procedure => MemoryKind::Procedure,
        };

        // Check for duplicates among existing active memories.
        let similar = memory
            .search(&extraction.content, 3)
            .await
            .context("failed to search for similar memories")?;

        let is_duplicate = similar.iter().any(|m| {
            normalize_for_compare(&m.content) == normalize_for_compare(&extraction.content)
        });

        if is_duplicate {
            duplicates = duplicates.saturating_add(1);
            debug!(content = %extraction.content, "observer skipping duplicate extraction");
            continue;
        }

        // Check for contradictions: similar active memories with different content.
        let has_contradiction = similar
            .iter()
            .filter(|m| m.kind == kind && m.status == MemoryStatus::Active)
            .any(|m| {
                let sim = word_overlap(&m.content, &extraction.content);
                // High word overlap but not identical = potential contradiction.
                sim > 0.4 && sim < 0.9
            });

        let metadata = if has_contradiction {
            contradictions = contradictions.saturating_add(1);
            Some(serde_json::json!({
                "session_id": session_id,
                "confidence": extraction.confidence,
                "contradiction": true,
            }))
        } else {
            Some(serde_json::json!({
                "session_id": session_id,
                "confidence": extraction.confidence,
            }))
        };

        let mem = Memory {
            id: None,
            kind,
            content: extraction.content.clone(),
            metadata,
            status: MemoryStatus::Pending,
            source: MemorySource::Observer,
            created_at: None,
            updated_at: None,
        };

        memory
            .save_memory(mem)
            .await
            .context("failed to save pending memory")?;
        staged = staged.saturating_add(1);
    }

    Ok(StagingResult {
        staged,
        duplicates,
        contradictions,
    })
}

/// Check pending memories for promotion.
///
/// - **Auto**: promotes memories that have been extracted at least `threshold` times.
/// - **Suggest**: sends a Telegram message listing pending memories for review.
/// - **Off**: no-op (should not be called, but safe).
///
/// # Errors
///
/// Returns an error if memory operations fail.
pub async fn check_promotions(
    memory: &MemoryEngine,
    config: &LearningConfig,
    telegram_tx: &mpsc::Sender<TelegramOutbound>,
    user_id: i64,
) -> anyhow::Result<PromotionResult> {
    match config.promotion_mode {
        PromotionMode::Off => Ok(PromotionResult {
            promoted: 0,
            suggested: 0,
        }),
        PromotionMode::Auto => auto_promote(memory, config.auto_promote_threshold).await,
        PromotionMode::Suggest => suggest_promote(memory, telegram_tx, user_id).await,
    }
}

/// Auto-promote pending memories that appear consistently.
///
/// Counts pending memories with similar content. When the count reaches
/// the threshold, promotes to active. Tracks already-promoted IDs to
/// avoid double-counting similar memories in the same batch.
async fn auto_promote(memory: &MemoryEngine, threshold: u32) -> anyhow::Result<PromotionResult> {
    let pending = memory
        .search_by_status(MemoryStatus::Pending, 100)
        .await
        .context("failed to search pending memories")?;

    if pending.is_empty() {
        return Ok(PromotionResult {
            promoted: 0,
            suggested: 0,
        });
    }

    let mut promoted: usize = 0;
    let mut promoted_ids = std::collections::HashSet::new();

    // Group similar pending memories and promote when count >= threshold.
    // Skip contradictions (they need manual review).
    for mem in &pending {
        // Skip if already promoted in this batch (avoids double-counting).
        if let Some(id) = mem.id {
            if promoted_ids.contains(&id) {
                continue;
            }
        }

        let is_contradiction = mem
            .metadata
            .as_ref()
            .and_then(|m| m.get("contradiction"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if is_contradiction {
            continue;
        }

        // Count similar pending memories (excluding already-promoted).
        let similar_count = pending
            .iter()
            .filter(|other| {
                other.id != mem.id
                    && !other.id.is_some_and(|id| promoted_ids.contains(&id))
                    && word_overlap(&other.content, &mem.content) > 0.7
            })
            .count();

        // +1 for the memory itself.
        let total_count = similar_count.saturating_add(1);

        #[allow(clippy::cast_possible_truncation)] // threshold u32 fits in usize
        if total_count >= threshold as usize {
            if let Some(id) = mem.id {
                memory
                    .update_memory_status(id, MemoryStatus::Active)
                    .await
                    .context("failed to promote memory")?;
                promoted_ids.insert(id);
                promoted = promoted.saturating_add(1);
                info!(id, content = %mem.content, "auto-promoted memory");
            }
        }
    }

    Ok(PromotionResult {
        promoted,
        suggested: 0,
    })
}

/// Send pending memories to user for manual review.
async fn suggest_promote(
    memory: &MemoryEngine,
    telegram_tx: &mpsc::Sender<TelegramOutbound>,
    user_id: i64,
) -> anyhow::Result<PromotionResult> {
    let pending = memory
        .search_by_status(MemoryStatus::Pending, 20)
        .await
        .context("failed to search pending memories")?;

    if pending.is_empty() {
        return Ok(PromotionResult {
            promoted: 0,
            suggested: 0,
        });
    }

    let mut lines = vec!["<b>Pending observer memories for review:</b>".to_owned()];
    for mem in &pending {
        let kind = mem.kind.as_str();
        let content = escape_html(&mem.content);
        let truncated = if content.len() > 100 {
            let t: String = content.chars().take(100).collect();
            format!("{t}...")
        } else {
            content
        };
        lines.push(format!("  [{kind}] {truncated}"));
    }
    lines.push("\nUse /memory_pending to manage these.".to_owned());

    let msg = TelegramOutbound {
        user_id,
        text: Some(lines.join("\n")),
        file_path: None,
        approval_keyboard: None,
    };

    if let Err(e) = telegram_tx.send(msg).await {
        warn!(error = %e, "failed to send observer suggestion to telegram");
    }

    Ok(PromotionResult {
        promoted: 0,
        suggested: pending.len(),
    })
}

/// Undo the last batch of promoted observer memories.
///
/// Finds recently activated memories with `source = Observer` and reverts
/// them to archived status.
///
/// # Errors
///
/// Returns an error if memory operations fail.
pub async fn undo_last_promotion(memory: &MemoryEngine) -> anyhow::Result<usize> {
    // Find active observer memories (most recently promoted).
    let active = memory
        .search_by_status(MemoryStatus::Active, 100)
        .await
        .context("failed to search active memories")?;

    let observer_active: Vec<_> = active
        .iter()
        .filter(|m| m.source == MemorySource::Observer)
        .collect();

    if observer_active.is_empty() {
        return Ok(0);
    }

    // Archive the most recent batch (last 10 observer-promoted memories).
    let batch_size = observer_active.len().min(10);
    let mut archived: usize = 0;

    for mem in observer_active.iter().take(batch_size) {
        if let Some(id) = mem.id {
            memory
                .update_memory_status(id, MemoryStatus::Archived)
                .await
                .context("failed to archive memory")?;
            archived = archived.saturating_add(1);
        }
    }

    info!(count = archived, "undid observer promotions");
    Ok(archived)
}

/// Normalize text for duplicate comparison (lowercase, trim whitespace).
fn normalize_for_compare(text: &str) -> String {
    text.trim().to_lowercase()
}

/// Simple word overlap ratio for contradiction/similarity detection.
///
/// Returns 0.0–1.0 where 1.0 means identical word sets.
fn word_overlap(a: &str, b: &str) -> f64 {
    let words_a: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let words_b: std::collections::HashSet<&str> = b.split_whitespace().collect();

    if words_a.is_empty() || words_b.is_empty() {
        return 0.0;
    }

    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();

    // union > 0 is guaranteed since both sets are non-empty (checked above).
    #[allow(clippy::cast_precision_loss)] // word counts are small enough for f64
    let ratio = (intersection as f64) / (union as f64);
    ratio
}
