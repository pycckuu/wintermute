//! Tests for `src/memory/writer.rs` — single-writer actor.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use wintermute::memory::{
    ConversationEntry, Memory, MemoryEngine, MemoryKind, MemorySource, MemoryStatus, TrustSource,
};

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

fn test_memory(content: &str) -> Memory {
    Memory {
        id: None,
        kind: MemoryKind::Fact,
        content: content.to_owned(),
        metadata: None,
        status: MemoryStatus::Active,
        source: MemorySource::User,
        created_at: None,
        updated_at: None,
    }
}

fn test_memory_with_status(content: &str, status: MemoryStatus) -> Memory {
    Memory {
        id: None,
        kind: MemoryKind::Fact,
        content: content.to_owned(),
        metadata: None,
        status,
        source: MemorySource::User,
        created_at: None,
        updated_at: None,
    }
}

#[tokio::test]
async fn save_memory_persists_to_database() {
    let engine = setup_engine().await;

    engine
        .save_memory(test_memory("the sky is blue"))
        .await
        .expect("save should succeed");

    // Give the writer actor time to process.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let row: (i64,) = sqlx::query_as("SELECT count(*) FROM memories")
        .fetch_one(engine.pool())
        .await
        .expect("count should succeed");
    assert_eq!(row.0, 1);

    engine.shutdown().await;
}

#[tokio::test]
async fn save_conversation_persists_to_database() {
    let engine = setup_engine().await;

    let entry = ConversationEntry {
        session_id: "sess-1".to_owned(),
        role: "user".to_owned(),
        content: "hello agent".to_owned(),
        tokens_used: Some(10),
    };
    engine
        .save_conversation(entry)
        .await
        .expect("save should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let row: (i64,) = sqlx::query_as("SELECT count(*) FROM conversations")
        .fetch_one(engine.pool())
        .await
        .expect("count should succeed");
    assert_eq!(row.0, 1);

    engine.shutdown().await;
}

#[tokio::test]
async fn update_memory_status_changes_status() {
    let engine = setup_engine().await;

    engine
        .save_memory(test_memory("pending fact"))
        .await
        .expect("save should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let (id,): (i64,) = sqlx::query_as("SELECT id FROM memories LIMIT 1")
        .fetch_one(engine.pool())
        .await
        .expect("should find a row");

    engine
        .update_memory_status(id, MemoryStatus::Archived)
        .await
        .expect("update should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let (status,): (String,) = sqlx::query_as("SELECT status FROM memories WHERE id = ?1")
        .bind(id)
        .fetch_one(engine.pool())
        .await
        .expect("query should succeed");

    assert_eq!(status, "archived");

    engine.shutdown().await;
}

#[tokio::test]
async fn trust_domain_persists_to_ledger() {
    let engine = setup_engine().await;

    engine
        .trust_domain("github.com", TrustSource::Config)
        .await
        .expect("trust should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let trusted = engine
        .is_domain_trusted("github.com")
        .await
        .expect("check should succeed");
    assert!(trusted);

    let untrusted = engine
        .is_domain_trusted("evil.com")
        .await
        .expect("check should succeed");
    assert!(!untrusted);

    engine.shutdown().await;
}

#[tokio::test]
async fn trust_domain_is_idempotent() {
    let engine = setup_engine().await;

    engine
        .trust_domain("api.github.com", TrustSource::Config)
        .await
        .expect("first trust should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Second insert for same domain should not fail (INSERT OR IGNORE).
    engine
        .trust_domain("api.github.com", TrustSource::User)
        .await
        .expect("duplicate trust should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let row: (i64,) = sqlx::query_as("SELECT count(*) FROM trust_ledger")
        .fetch_one(engine.pool())
        .await
        .expect("count should succeed");
    assert_eq!(row.0, 1, "should have exactly one row for the domain");

    engine.shutdown().await;
}

#[tokio::test]
async fn multiple_writes_are_serialized() {
    let engine = setup_engine().await;

    for i in 0..10 {
        engine
            .save_memory(test_memory(&format!("fact number {i}")))
            .await
            .expect("save should succeed");
    }

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let row: (i64,) = sqlx::query_as("SELECT count(*) FROM memories")
        .fetch_one(engine.pool())
        .await
        .expect("count should succeed");
    assert_eq!(row.0, 10);

    engine.shutdown().await;
}

#[tokio::test]
async fn search_conversations_returns_matching_session() {
    let engine = setup_engine().await;

    for (sid, content) in [("s1", "hello"), ("s1", "world"), ("s2", "other")] {
        engine
            .save_conversation(ConversationEntry {
                session_id: sid.to_owned(),
                role: "user".to_owned(),
                content: content.to_owned(),
                tokens_used: None,
            })
            .await
            .expect("save should succeed");
    }

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let results = engine
        .search_conversations("s1", 100)
        .await
        .expect("search should succeed");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].content, "hello");
    assert_eq!(results[1].content, "world");

    engine.shutdown().await;
}

#[tokio::test]
async fn save_memory_rejects_oversized_content() {
    let engine = setup_engine().await;

    let oversized = "x".repeat(wintermute::memory::MAX_CONTENT_SIZE + 1);
    let err = engine
        .save_memory(test_memory(&oversized))
        .await
        .expect_err("oversized content should be rejected");

    assert!(
        err.to_string().contains("content too large"),
        "expected ContentTooLarge error, got: {err}"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn save_conversation_rejects_oversized_content() {
    let engine = setup_engine().await;

    let oversized = "x".repeat(wintermute::memory::MAX_CONTENT_SIZE + 1);
    let entry = ConversationEntry {
        session_id: "sess-big".to_owned(),
        role: "user".to_owned(),
        content: oversized,
        tokens_used: None,
    };
    let err = engine
        .save_conversation(entry)
        .await
        .expect_err("oversized content should be rejected");

    assert!(
        err.to_string().contains("content too large"),
        "expected ContentTooLarge error, got: {err}"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn search_by_status_returns_pending_memories() {
    let engine = setup_engine().await;

    engine
        .save_memory(test_memory_with_status(
            "active fact one",
            MemoryStatus::Active,
        ))
        .await
        .expect("save should succeed");
    engine
        .save_memory(test_memory_with_status(
            "pending fact one",
            MemoryStatus::Pending,
        ))
        .await
        .expect("save should succeed");
    engine
        .save_memory(test_memory_with_status(
            "pending fact two",
            MemoryStatus::Pending,
        ))
        .await
        .expect("save should succeed");
    engine
        .save_memory(test_memory_with_status(
            "archived fact one",
            MemoryStatus::Archived,
        ))
        .await
        .expect("save should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let pending = engine
        .search_by_status(MemoryStatus::Pending, 100)
        .await
        .expect("search_by_status should succeed");
    assert_eq!(pending.len(), 2, "should return exactly 2 pending memories");
    for m in &pending {
        assert_eq!(m.status, MemoryStatus::Pending);
    }

    let active = engine
        .search_by_status(MemoryStatus::Active, 100)
        .await
        .expect("search_by_status should succeed");
    assert_eq!(active.len(), 1, "should return exactly 1 active memory");
    assert_eq!(active[0].status, MemoryStatus::Active);

    let archived = engine
        .search_by_status(MemoryStatus::Archived, 100)
        .await
        .expect("search_by_status should succeed");
    assert_eq!(archived.len(), 1, "should return exactly 1 archived memory");

    engine.shutdown().await;
}

#[tokio::test]
async fn delete_memory_removes_from_database() {
    let engine = setup_engine().await;

    engine
        .save_memory(test_memory("ephemeral fact"))
        .await
        .expect("save should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let (id,): (i64,) = sqlx::query_as("SELECT id FROM memories LIMIT 1")
        .fetch_one(engine.pool())
        .await
        .expect("should find the saved memory");

    engine
        .delete_memory(id)
        .await
        .expect("delete should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let row: (i64,) = sqlx::query_as("SELECT count(*) FROM memories")
        .fetch_one(engine.pool())
        .await
        .expect("count should succeed");
    assert_eq!(row.0, 0, "memory should be deleted from database");

    // FTS5 index should also be clean (trigger handles this).
    let fts_rows: Vec<(i64,)> =
        sqlx::query_as("SELECT rowid FROM memories_fts WHERE memories_fts MATCH 'ephemeral'")
            .fetch_all(engine.pool())
            .await
            .expect("fts query should succeed");
    assert!(
        fts_rows.is_empty(),
        "FTS5 index should be cleared after delete"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn count_by_status_returns_correct_count() {
    let engine = setup_engine().await;

    engine
        .save_memory(test_memory_with_status("active one", MemoryStatus::Active))
        .await
        .expect("save should succeed");
    engine
        .save_memory(test_memory_with_status("active two", MemoryStatus::Active))
        .await
        .expect("save should succeed");
    engine
        .save_memory(test_memory_with_status(
            "pending one",
            MemoryStatus::Pending,
        ))
        .await
        .expect("save should succeed");
    engine
        .save_memory(test_memory_with_status(
            "archived one",
            MemoryStatus::Archived,
        ))
        .await
        .expect("save should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let active_count = engine
        .count_by_status(MemoryStatus::Active)
        .await
        .expect("count should succeed");
    assert_eq!(active_count, 2);

    let pending_count = engine
        .count_by_status(MemoryStatus::Pending)
        .await
        .expect("count should succeed");
    assert_eq!(pending_count, 1);

    let archived_count = engine
        .count_by_status(MemoryStatus::Archived)
        .await
        .expect("count should succeed");
    assert_eq!(archived_count, 1);

    engine.shutdown().await;
}

#[tokio::test]
async fn db_size_bytes_returns_nonzero() {
    let engine = setup_engine().await;

    let size = engine
        .db_size_bytes()
        .await
        .expect("db_size_bytes should succeed");
    assert!(size > 0, "database should have non-zero size, got {size}");

    engine.shutdown().await;
}
