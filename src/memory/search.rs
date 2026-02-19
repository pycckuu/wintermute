//! FTS5 full-text search with optional vector similarity and RRF merging.
//!
//! When no [`Embedder`] is configured, search uses
//! SQLite FTS5 only. When an embedder is available, results from FTS5 and
//! vector similarity are merged using Reciprocal Rank Fusion (RRF).

use sqlx::SqlitePool;

use super::embedder::Embedder;
use super::{Memory, MemoryError, MemoryKind, MemorySource, MemoryStatus};

/// Raw row returned by the FTS5 query.
///
/// Fields: `(id, kind, content, metadata, status, source, created_at, updated_at)`.
type MemoryRow = (
    i64,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
);

/// Search active memories using FTS5 and optional vector similarity.
///
/// Returns up to `limit` results ranked by relevance. Only memories with
/// `status = 'active'` are returned.
pub async fn search(
    db: &SqlitePool,
    _embedder: Option<&dyn Embedder>,
    query: &str,
    limit: usize,
) -> Result<Vec<Memory>, MemoryError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }

    let fts_results = fts5_search(db, query, limit).await?;

    // Vector search integration deferred — sqlite-vec extension loading
    // requires runtime configuration that will be added in a follow-up.
    // For now, FTS5 is the sole search path.

    Ok(fts_results)
}

/// Full-text search via FTS5 MATCH.
///
/// Sanitises the query for FTS5 syntax, then joins against the `memories`
/// table to return full [`Memory`] rows ordered by FTS5 rank.
async fn fts5_search(
    db: &SqlitePool,
    query: &str,
    limit: usize,
) -> Result<Vec<Memory>, MemoryError> {
    let sanitised = sanitise_fts5_query(query);
    if sanitised.is_empty() {
        return Ok(Vec::new());
    }

    let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);

    let rows: Vec<MemoryRow> = sqlx::query_as(
        "SELECT m.id, m.kind, m.content, m.metadata, m.status, m.source, \
                    m.created_at, m.updated_at \
             FROM memories_fts f \
             JOIN memories m ON f.rowid = m.id \
             WHERE memories_fts MATCH ?1 \
               AND m.status = 'active' \
             ORDER BY f.rank \
             LIMIT ?2",
    )
    .bind(&sanitised)
    .bind(limit_i64)
    .fetch_all(db)
    .await?;

    rows.into_iter().map(row_to_memory).collect()
}

/// Convert a raw query row tuple into a [`Memory`].
fn row_to_memory(row: MemoryRow) -> Result<Memory, MemoryError> {
    let (id, kind_str, content, metadata_str, status_str, source_str, created_at, updated_at) = row;
    let metadata = metadata_str
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| MemoryError::Database(sqlx::Error::Decode(Box::new(e))))?;

    Ok(Memory {
        id: Some(id),
        kind: MemoryKind::parse(&kind_str)?,
        content,
        metadata,
        status: MemoryStatus::parse(&status_str)?,
        source: MemorySource::parse(&source_str)?,
        created_at: Some(created_at),
        updated_at: Some(updated_at),
    })
}

/// Sanitise a user query string for FTS5 MATCH syntax.
///
/// FTS5 treats certain characters as operators. We strip them to avoid
/// syntax errors while preserving the search intent.
fn sanitise_fts5_query(query: &str) -> String {
    // Remove FTS5 special characters and collapse whitespace.
    let cleaned: String = query
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '_' {
                c
            } else {
                ' '
            }
        })
        .collect();

    // FTS5 keyword operators that cause parse errors when used as search terms.
    const FTS5_KEYWORDS: &[&str] = &["OR", "NOT", "AND", "NEAR"];

    let tokens: Vec<&str> = cleaned
        .split_whitespace()
        .filter(|t| !FTS5_KEYWORDS.contains(t))
        .collect();
    if tokens.is_empty() {
        return String::new();
    }

    // Join tokens with spaces — FTS5 treats them as implicit AND.
    tokens.join(" ")
}

// ---------------------------------------------------------------------------
// RRF merge (ready for when vector search is enabled)
// ---------------------------------------------------------------------------

/// Reciprocal Rank Fusion constant (standard value).
#[allow(dead_code)]
const RRF_K: f64 = 60.0;

/// Merge two ranked result lists using Reciprocal Rank Fusion.
///
/// Each item's score is `1/(k + rank)` from each list, summed. Results are
/// returned in descending score order, truncated to `limit`.
#[allow(dead_code)]
pub(crate) fn rrf_merge(list_a: Vec<Memory>, list_b: Vec<Memory>, limit: usize) -> Vec<Memory> {
    use std::collections::HashMap;

    let mut scores: HashMap<i64, (f64, Option<Memory>)> = HashMap::new();

    // Accumulate RRF scores from both lists.
    for list in [list_a, list_b] {
        for (rank, memory) in list.into_iter().enumerate() {
            let id = memory.id.unwrap_or(-1);
            // u32::try_from is safe: search results never exceed 2^32 items.
            let rank_u32 = u32::try_from(rank).unwrap_or(u32::MAX);
            let score = 1.0 / (RRF_K + f64::from(rank_u32));
            let entry = scores.entry(id).or_insert((0.0, None));
            entry.0 += score;
            if entry.1.is_none() {
                entry.1 = Some(memory);
            }
        }
    }

    let mut merged: Vec<(f64, Memory)> = scores
        .into_values()
        .filter_map(|(score, mem)| mem.map(|m| (score, m)))
        .collect();

    // Sort by score descending (highest first).
    merged.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    merged.into_iter().take(limit).map(|(_, m)| m).collect()
}
