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
