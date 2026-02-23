//! Tests for `src/heartbeat/backup.rs` — script copy and memory VACUUM INTO.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[tokio::test]
async fn backup_creates_timestamped_directory() {
    let tmp = tempfile::tempdir().expect("should create temp dir");
    let scripts_dir = tmp.path().join("scripts");
    let backups_dir = tmp.path().join("backups");

    std::fs::create_dir_all(&scripts_dir).expect("should create scripts dir");
    std::fs::write(scripts_dir.join("tool.json"), "{}").expect("should write tool file");

    // Create in-memory SQLite pool.
    let pool = create_test_pool().await;

    let result = wintermute::heartbeat::backup::create_backup(&scripts_dir, &pool, &backups_dir)
        .await
        .expect("backup should succeed");

    assert!(result.backup_dir.exists(), "backup dir should exist");
    assert!(result.scripts_copied, "scripts should be copied");
    assert!(
        result.total_size_bytes > 0,
        "backup should have non-zero size"
    );
}

#[tokio::test]
async fn backup_handles_missing_scripts_dir() {
    let tmp = tempfile::tempdir().expect("should create temp dir");
    let scripts_dir = tmp.path().join("nonexistent_scripts");
    let backups_dir = tmp.path().join("backups");

    let pool = create_test_pool().await;

    let result = wintermute::heartbeat::backup::create_backup(&scripts_dir, &pool, &backups_dir)
        .await
        .expect("backup should succeed even without scripts dir");

    assert!(!result.scripts_copied, "scripts_copied should be false");
}

#[tokio::test]
async fn backup_copies_nested_directory_structure() {
    let tmp = tempfile::tempdir().expect("should create temp dir");
    let scripts_dir = tmp.path().join("scripts");
    let backups_dir = tmp.path().join("backups");

    // Create nested structure.
    let nested = scripts_dir.join("subdir");
    std::fs::create_dir_all(&nested).expect("should create nested dir");
    std::fs::write(nested.join("nested.json"), "{}").expect("should write nested file");
    std::fs::write(scripts_dir.join("root.json"), "{}").expect("should write root file");

    let pool = create_test_pool().await;

    let result = wintermute::heartbeat::backup::create_backup(&scripts_dir, &pool, &backups_dir)
        .await
        .expect("backup should succeed");

    assert!(result.scripts_copied);

    let backup_nested = result.backup_dir.join("scripts/subdir/nested.json");
    assert!(
        backup_nested.exists(),
        "nested file should be preserved in backup"
    );

    let backup_root = result.backup_dir.join("scripts/root.json");
    assert!(
        backup_root.exists(),
        "root file should be preserved in backup"
    );
}

#[tokio::test]
async fn backup_rejects_path_with_sql_dangerous_characters() {
    let tmp = tempfile::tempdir().expect("should create temp dir");
    let scripts_dir = tmp.path().join("scripts");
    std::fs::create_dir_all(&scripts_dir).expect("should create scripts dir");

    let pool = create_test_pool().await;

    // Paths with characters outside the allowlist trigger a VACUUM INTO
    // rejection. create_backup catches the error and sets memory_copied = false.
    let backups_dir = tmp.path().join("back'up");
    let result =
        wintermute::heartbeat::backup::create_backup(&scripts_dir, &pool, &backups_dir).await;
    // create_backup returns Ok even if VACUUM INTO fails (it logs a warning).
    let backup = result.expect("create_backup should succeed overall");
    assert!(
        !backup.memory_copied,
        "memory_copied should be false when path contains disallowed chars"
    );
}

async fn create_test_pool() -> sqlx::SqlitePool {
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

    pool
}
