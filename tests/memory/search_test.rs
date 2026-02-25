//! Tests for `src/memory/search.rs` — FTS5 search, fallback behaviour, and RRF merge.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use wintermute::memory::{Memory, MemoryEngine, MemoryKind, MemorySource, MemoryStatus};

/// Create an in-memory [`MemoryEngine`] with all migrations applied.
async fn setup_engine() -> MemoryEngine {
    let opts = SqliteConnectOptions::new()
        .filename(":memory:")
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("pool should connect");

    let bootstrap = include_str!("../../migrations/001_schema.sql");
    sqlx::raw_sql(bootstrap)
        .execute(&pool)
        .await
        .expect("001 should apply");

    let memory_sql = include_str!("../../migrations/002_memory.sql");
    sqlx::raw_sql(memory_sql)
        .execute(&pool)
        .await
        .expect("002 should apply");

    MemoryEngine::new(pool, None)
        .await
        .expect("engine should initialise")
}

/// Build an active memory with the given content and kind (other fields defaulted).
fn test_memory(content: &str, kind: MemoryKind) -> Memory {
    Memory {
        id: None,
        kind,
        content: content.to_owned(),
        metadata: None,
        status: MemoryStatus::Active,
        source: MemorySource::User,
        created_at: None,
        updated_at: None,
    }
}

/// Save all memories and wait for the writer actor to flush.
async fn seed_and_wait(engine: &MemoryEngine, memories: Vec<Memory>) {
    for m in memories {
        engine.save_memory(m).await.expect("save should succeed");
    }
    // Give the writer actor time to flush (writes are async via mpsc).
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn search_returns_matching_memories() {
    let engine = setup_engine().await;

    seed_and_wait(
        &engine,
        vec![
            test_memory("rust programming language", MemoryKind::Fact),
            test_memory("python scripting language", MemoryKind::Fact),
            test_memory("rust is fast and safe", MemoryKind::Procedure),
        ],
    )
    .await;

    let results = engine
        .search("rust", 10)
        .await
        .expect("search should succeed");
    assert!(
        results.len() >= 2,
        "should match at least 2 rust-related memories, got {}",
        results.len()
    );

    for result in &results {
        assert!(
            result.content.to_lowercase().contains("rust"),
            "result should contain search term"
        );
    }

    engine.shutdown().await;
}

#[tokio::test]
async fn search_excludes_archived_memories() {
    let engine = setup_engine().await;

    engine
        .save_memory(test_memory(
            "archived content about databases",
            MemoryKind::Fact,
        ))
        .await
        .expect("save should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Archive the memory.
    let (id,): (i64,) = sqlx::query_as("SELECT id FROM memories LIMIT 1")
        .fetch_one(engine.pool())
        .await
        .expect("should find a row");

    engine
        .update_memory_status(id, MemoryStatus::Archived)
        .await
        .expect("update should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let results = engine
        .search("databases", 10)
        .await
        .expect("search should succeed");
    assert!(
        results.is_empty(),
        "archived memories should not appear in results"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn search_respects_limit() {
    let engine = setup_engine().await;

    seed_and_wait(
        &engine,
        vec![
            test_memory("machine learning basics", MemoryKind::Fact),
            test_memory("machine learning advanced", MemoryKind::Fact),
            test_memory("machine learning intermediate", MemoryKind::Fact),
        ],
    )
    .await;

    let results = engine
        .search("machine learning", 2)
        .await
        .expect("search should succeed");
    assert!(
        results.len() <= 2,
        "should respect limit, got {}",
        results.len()
    );

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// Fallback to recent active memories (cognitive cold start prevention)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_empty_query_falls_back_to_recent_active() {
    let engine = setup_engine().await;

    seed_and_wait(&engine, vec![test_memory("some content", MemoryKind::Fact)]).await;

    let results = engine.search("", 10).await.expect("search should succeed");
    assert_eq!(
        results.len(),
        1,
        "empty query should fall back to recent active memories"
    );
    assert_eq!(results[0].content, "some content");

    engine.shutdown().await;
}

#[tokio::test]
async fn search_empty_query_with_no_memories_returns_empty() {
    let engine = setup_engine().await;

    let results = engine.search("", 10).await.expect("search should succeed");
    assert!(
        results.is_empty(),
        "empty query with no memories should return empty"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn search_wildcard_falls_back_to_recent_active() {
    let engine = setup_engine().await;

    seed_and_wait(
        &engine,
        vec![test_memory("important user fact", MemoryKind::Fact)],
    )
    .await;

    // "*" gets stripped by sanitise_fts5_query → empty → fallback to recent
    let results = engine.search("*", 10).await.expect("search should succeed");
    assert_eq!(
        results.len(),
        1,
        "wildcard query should fall back to recent active memories"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn search_no_fts_matches_falls_back_to_recent_active() {
    let engine = setup_engine().await;

    seed_and_wait(
        &engine,
        vec![test_memory("cats and dogs", MemoryKind::Fact)],
    )
    .await;

    let results = engine
        .search("quantum", 10)
        .await
        .expect("search should succeed");
    assert_eq!(
        results.len(),
        1,
        "non-matching query should fall back to recent active memories"
    );
    assert_eq!(results[0].content, "cats and dogs");

    engine.shutdown().await;
}

#[tokio::test]
async fn search_fallback_respects_limit() {
    let engine = setup_engine().await;

    seed_and_wait(
        &engine,
        vec![
            test_memory("memory one", MemoryKind::Fact),
            test_memory("memory two", MemoryKind::Fact),
            test_memory("memory three", MemoryKind::Fact),
            test_memory("memory four", MemoryKind::Fact),
            test_memory("memory five", MemoryKind::Fact),
        ],
    )
    .await;

    // Query that won't match any memory, triggering fallback.
    let results = engine
        .search("zzzznonexistent", 2)
        .await
        .expect("search should succeed");
    assert!(
        results.len() <= 2,
        "fallback should respect limit, got {}",
        results.len()
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn search_handles_special_characters_gracefully() {
    let engine = setup_engine().await;

    seed_and_wait(
        &engine,
        vec![test_memory("test content for safety", MemoryKind::Fact)],
    )
    .await;

    // FTS5 special chars that could cause parse errors.
    let results = engine
        .search("test OR NOT *", 10)
        .await
        .expect("search with special chars should not fail");
    // May or may not match — the point is it doesn't error.
    let _ = results;

    engine.shutdown().await;
}

#[tokio::test]
async fn search_returns_correct_memory_fields() {
    let engine = setup_engine().await;

    let input = Memory {
        id: None,
        kind: MemoryKind::Procedure,
        content: "deploy the application to production".to_owned(),
        metadata: Some(serde_json::json!({"tag": "ops"})),
        status: MemoryStatus::Active,
        source: MemorySource::Agent,
        created_at: None,
        updated_at: None,
    };
    engine
        .save_memory(input)
        .await
        .expect("save should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let results = engine
        .search("deploy production", 10)
        .await
        .expect("search should succeed");

    assert_eq!(results.len(), 1);
    let result = &results[0];
    assert!(result.id.is_some());
    assert_eq!(result.kind, MemoryKind::Procedure);
    assert_eq!(result.content, "deploy the application to production");
    assert_eq!(result.status, MemoryStatus::Active);
    assert_eq!(result.source, MemorySource::Agent);
    assert!(result.created_at.is_some());
    assert!(result.updated_at.is_some());

    engine.shutdown().await;
}
