//! Tests for `migrations/002_memory.sql` applying cleanly.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

async fn fresh_pool() -> SqlitePool {
    let opts = SqliteConnectOptions::new()
        .filename(":memory:")
        .create_if_missing(true);
    // In-memory databases are per-connection, so limit to 1 connection
    // to ensure migrations and queries share the same database.
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("in-memory pool should connect")
}

async fn apply_migrations(pool: &SqlitePool) {
    let bootstrap = include_str!("../../migrations/001_schema.sql");
    sqlx::raw_sql(bootstrap)
        .execute(pool)
        .await
        .expect("001 should apply");

    let memory = include_str!("../../migrations/002_memory.sql");
    sqlx::raw_sql(memory)
        .execute(pool)
        .await
        .expect("002 should apply");
}

#[tokio::test]
async fn migration_applies_on_fresh_database() {
    let pool = fresh_pool().await;
    apply_migrations(&pool).await;
}

#[tokio::test]
async fn migration_creates_memories_table() {
    let pool = fresh_pool().await;
    apply_migrations(&pool).await;

    sqlx::query(
        "INSERT INTO memories (kind, content, status, source) \
         VALUES ('fact', 'test content', 'active', 'user')",
    )
    .execute(&pool)
    .await
    .expect("insert into memories should succeed");

    let row: (i64,) = sqlx::query_as("SELECT count(*) FROM memories")
        .fetch_one(&pool)
        .await
        .expect("count query should succeed");
    assert_eq!(row.0, 1);
}

#[tokio::test]
async fn migration_creates_conversations_table() {
    let pool = fresh_pool().await;
    apply_migrations(&pool).await;

    sqlx::query(
        "INSERT INTO conversations (session_id, role, content) \
         VALUES ('s1', 'user', 'hello')",
    )
    .execute(&pool)
    .await
    .expect("insert into conversations should succeed");
}

#[tokio::test]
async fn migration_creates_trust_ledger() {
    let pool = fresh_pool().await;
    apply_migrations(&pool).await;

    sqlx::query("INSERT INTO trust_ledger (domain, approved_by) VALUES ('example.com', 'config')")
        .execute(&pool)
        .await
        .expect("insert into trust_ledger should succeed");
}

#[tokio::test]
async fn migration_creates_fts5_tables() {
    let pool = fresh_pool().await;
    apply_migrations(&pool).await;

    // Insert a memory and verify the FTS trigger populated the index.
    sqlx::query(
        "INSERT INTO memories (kind, content, status, source) \
         VALUES ('fact', 'quantum computing basics', 'active', 'user')",
    )
    .execute(&pool)
    .await
    .expect("insert should succeed");

    let rows: Vec<(i64,)> =
        sqlx::query_as("SELECT rowid FROM memories_fts WHERE memories_fts MATCH 'quantum'")
            .fetch_all(&pool)
            .await
            .expect("fts match should succeed");
    assert_eq!(rows.len(), 1, "FTS trigger should have indexed the row");
}

#[tokio::test]
async fn migration_check_constraints_reject_invalid_kind() {
    let pool = fresh_pool().await;
    apply_migrations(&pool).await;

    let result = sqlx::query(
        "INSERT INTO memories (kind, content, status, source) \
         VALUES ('invalid_kind', 'test', 'active', 'user')",
    )
    .execute(&pool)
    .await;

    assert!(result.is_err(), "invalid kind should be rejected by CHECK");
}

#[tokio::test]
async fn migration_check_constraints_reject_invalid_status() {
    let pool = fresh_pool().await;
    apply_migrations(&pool).await;

    let result = sqlx::query(
        "INSERT INTO memories (kind, content, status, source) \
         VALUES ('fact', 'test', 'deleted', 'user')",
    )
    .execute(&pool)
    .await;

    assert!(
        result.is_err(),
        "invalid status should be rejected by CHECK"
    );
}

#[tokio::test]
async fn migration_is_idempotent() {
    let pool = fresh_pool().await;
    apply_migrations(&pool).await;
    // Applying again should not fail (IF NOT EXISTS).
    apply_migrations(&pool).await;
}
