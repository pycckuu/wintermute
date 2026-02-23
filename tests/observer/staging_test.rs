//! Tests for `src/observer/staging.rs` — staging, dedup, contradiction, promotion.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use wintermute::config::{LearningConfig, PromotionMode};
use wintermute::memory::{Memory, MemoryEngine, MemoryKind, MemorySource, MemoryStatus};
use wintermute::observer::extractor::{Extraction, ExtractionKind};
use wintermute::observer::staging::{check_promotions, stage_extractions, undo_last_promotion};

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

fn fact(content: &str, confidence: f64) -> Extraction {
    Extraction {
        kind: ExtractionKind::Fact,
        content: content.to_owned(),
        confidence,
    }
}

fn procedure(content: &str) -> Extraction {
    Extraction {
        kind: ExtractionKind::Procedure,
        content: content.to_owned(),
        confidence: 0.8,
    }
}

#[tokio::test]
async fn stage_new_extractions_as_pending() {
    let engine = setup_engine().await;

    let extractions = vec![
        fact("user prefers dark mode", 0.8),
        procedure("deploy with cargo build --release"),
    ];

    let result = stage_extractions(&extractions, &engine, "sess-1")
        .await
        .expect("staging should succeed");

    assert_eq!(result.staged, 2);
    assert_eq!(result.duplicates, 0);
    assert_eq!(result.contradictions, 0);

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let pending = engine
        .search_by_status(MemoryStatus::Pending, 100)
        .await
        .expect("search should succeed");
    assert_eq!(pending.len(), 2);
    for m in &pending {
        assert_eq!(m.source, MemorySource::Observer);
        assert_eq!(m.status, MemoryStatus::Pending);
    }

    engine.shutdown().await;
}

#[tokio::test]
async fn stage_detects_duplicate_extractions() {
    let engine = setup_engine().await;

    // Save an existing active memory.
    engine
        .save_memory(Memory {
            id: None,
            kind: MemoryKind::Fact,
            content: "user prefers dark mode".to_owned(),
            metadata: None,
            status: MemoryStatus::Active,
            source: MemorySource::User,
            created_at: None,
            updated_at: None,
        })
        .await
        .expect("save should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let extractions = vec![fact("user prefers dark mode", 0.8)];

    let result = stage_extractions(&extractions, &engine, "sess-1")
        .await
        .expect("staging should succeed");

    assert_eq!(result.staged, 0);
    assert_eq!(result.duplicates, 1);

    engine.shutdown().await;
}

#[tokio::test]
async fn stage_empty_extractions() {
    let engine = setup_engine().await;

    let result = stage_extractions(&[], &engine, "sess-1")
        .await
        .expect("staging should succeed");

    assert_eq!(result.staged, 0);
    assert_eq!(result.duplicates, 0);
    assert_eq!(result.contradictions, 0);

    engine.shutdown().await;
}

#[tokio::test]
async fn undo_last_promotion_reverts_observer_memories() {
    let engine = setup_engine().await;

    // Save some observer-promoted active memories.
    for i in 0..3 {
        engine
            .save_memory(Memory {
                id: None,
                kind: MemoryKind::Fact,
                content: format!("observer fact {i}"),
                metadata: None,
                status: MemoryStatus::Active,
                source: MemorySource::Observer,
                created_at: None,
                updated_at: None,
            })
            .await
            .expect("save should succeed");
    }

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let reverted = undo_last_promotion(&engine)
        .await
        .expect("undo should succeed");
    assert_eq!(reverted, 3);

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let active = engine
        .search_by_status(MemoryStatus::Active, 100)
        .await
        .expect("search should succeed");
    let observer_active: Vec<_> = active
        .iter()
        .filter(|m| m.source == MemorySource::Observer)
        .collect();
    assert!(
        observer_active.is_empty(),
        "no observer memories should remain active"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn undo_with_no_observer_memories_returns_zero() {
    let engine = setup_engine().await;

    let reverted = undo_last_promotion(&engine)
        .await
        .expect("undo should succeed");
    assert_eq!(reverted, 0);

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// check_promotions tests
// ---------------------------------------------------------------------------

fn auto_config(threshold: u32) -> LearningConfig {
    LearningConfig {
        enabled: true,
        promotion_mode: PromotionMode::Auto,
        auto_promote_threshold: threshold,
    }
}

#[tokio::test]
async fn auto_promote_promotes_when_threshold_reached() {
    let engine = setup_engine().await;
    let (tx, _rx) = tokio::sync::mpsc::channel(16);

    // Stage 3 similar extractions (threshold = 2).
    let extractions = vec![
        fact("the user prefers dark mode", 0.9),
        fact("the user prefers dark mode", 0.85),
        fact("the user prefers dark mode", 0.8),
    ];
    stage_extractions(&extractions, &engine, "sess-1")
        .await
        .expect("staging should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let result = check_promotions(&engine, &auto_config(2), &tx, 12345)
        .await
        .expect("promotion should succeed");

    assert!(result.promoted > 0, "should promote at least one memory");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let active = engine
        .search_by_status(MemoryStatus::Active, 100)
        .await
        .expect("search should succeed");
    assert!(
        !active.is_empty(),
        "should have active memories after promotion"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn auto_promote_skips_contradictions() {
    let engine = setup_engine().await;
    let (tx, _rx) = tokio::sync::mpsc::channel(16);

    // Save a pending memory marked as contradiction.
    engine
        .save_memory(Memory {
            id: None,
            kind: MemoryKind::Fact,
            content: "user prefers light mode".to_owned(),
            metadata: Some(serde_json::json!({"contradiction": true})),
            status: MemoryStatus::Pending,
            source: MemorySource::Observer,
            created_at: None,
            updated_at: None,
        })
        .await
        .expect("save should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let result = check_promotions(&engine, &auto_config(1), &tx, 12345)
        .await
        .expect("promotion should succeed");

    assert_eq!(result.promoted, 0, "contradictions should not be promoted");

    engine.shutdown().await;
}

#[tokio::test]
async fn promotion_mode_off_is_noop() {
    let engine = setup_engine().await;
    let (tx, _rx) = tokio::sync::mpsc::channel(16);

    let off_config = LearningConfig {
        enabled: true,
        promotion_mode: PromotionMode::Off,
        auto_promote_threshold: 1,
    };

    let result = check_promotions(&engine, &off_config, &tx, 12345)
        .await
        .expect("promotion should succeed");

    assert_eq!(result.promoted, 0);
    assert_eq!(result.suggested, 0);

    engine.shutdown().await;
}
