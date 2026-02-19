//! Approval manager tests.

use wintermute::agent::approval::{ApprovalManager, ApprovalResult};

#[test]
fn request_returns_8_char_id() {
    let mgr = ApprovalManager::new();
    let id = mgr.request(
        "web_request".to_owned(),
        serde_json::json!({"url": "https://example.com"}),
        "session-1".to_owned(),
        12345,
    );
    assert_eq!(id.len(), 8);
    assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
}

#[test]
fn resolve_approved_returns_correct_data() {
    let mgr = ApprovalManager::new();
    let id = mgr.request(
        "web_request".to_owned(),
        serde_json::json!({"url": "https://example.com"}),
        "session-1".to_owned(),
        12345,
    );

    let result = mgr.resolve(&id, true, 12345);
    match result {
        ApprovalResult::Approved {
            session_id,
            tool_name,
            ..
        } => {
            assert_eq!(session_id, "session-1");
            assert_eq!(tool_name, "web_request");
        }
        other => panic!("expected Approved, got {other:?}"),
    }
}

#[test]
fn resolve_denied_returns_denied() {
    let mgr = ApprovalManager::new();
    let id = mgr.request(
        "web_request".to_owned(),
        serde_json::json!({}),
        "session-1".to_owned(),
        12345,
    );

    let result = mgr.resolve(&id, false, 12345);
    match result {
        ApprovalResult::Denied {
            session_id,
            tool_name,
        } => {
            assert_eq!(session_id, "session-1");
            assert_eq!(tool_name, "web_request");
        }
        other => panic!("expected Denied, got {other:?}"),
    }
}

#[test]
fn resolve_wrong_user_returns_wrong_user() {
    let mgr = ApprovalManager::new();
    let id = mgr.request(
        "web_request".to_owned(),
        serde_json::json!({}),
        "session-1".to_owned(),
        12345,
    );

    let result = mgr.resolve(&id, true, 99999);
    assert_eq!(result, ApprovalResult::WrongUser);
}

#[test]
fn resolve_expired_returns_expired() {
    let mgr = ApprovalManager::new();
    let id = mgr.request(
        "web_request".to_owned(),
        serde_json::json!({}),
        "session-1".to_owned(),
        12345,
    );

    // Manually set the expiry to the past
    if let Ok(mut map) = mgr.pending_map() {
        if let Some(entry) = map.get_mut(&id) {
            entry.expires_at = chrono::Utc::now() - chrono::Duration::minutes(1);
        }
    }

    let result = mgr.resolve(&id, true, 12345);
    assert_eq!(result, ApprovalResult::Expired);
}

#[test]
fn resolve_unknown_id_returns_not_found() {
    let mgr = ApprovalManager::new();
    let result = mgr.resolve("nonexist", true, 12345);
    assert_eq!(result, ApprovalResult::NotFound);
}

#[test]
fn resolve_is_single_use() {
    let mgr = ApprovalManager::new();
    let id = mgr.request(
        "web_request".to_owned(),
        serde_json::json!({}),
        "session-1".to_owned(),
        12345,
    );

    let first = mgr.resolve(&id, true, 12345);
    assert!(matches!(first, ApprovalResult::Approved { .. }));

    let second = mgr.resolve(&id, true, 12345);
    assert_eq!(second, ApprovalResult::NotFound);
}

#[test]
fn gc_expired_removes_only_expired() {
    let mgr = ApprovalManager::new();

    let expired_id = mgr.request(
        "web_request".to_owned(),
        serde_json::json!({}),
        "session-1".to_owned(),
        12345,
    );

    let active_id = mgr.request(
        "memory_save".to_owned(),
        serde_json::json!({}),
        "session-2".to_owned(),
        12345,
    );

    // Force the first entry to expire
    if let Ok(mut map) = mgr.pending_map() {
        if let Some(entry) = map.get_mut(&expired_id) {
            entry.expires_at = chrono::Utc::now() - chrono::Duration::minutes(1);
        }
    }

    mgr.gc_expired();

    // Expired one should be gone
    let result = mgr.resolve(&expired_id, true, 12345);
    assert_eq!(result, ApprovalResult::NotFound);

    // Active one should still exist
    let result = mgr.resolve(&active_id, true, 12345);
    assert!(matches!(result, ApprovalResult::Approved { .. }));
}

#[test]
fn pending_count_returns_correct_count_per_session() {
    let mgr = ApprovalManager::new();

    mgr.request(
        "tool_a".to_owned(),
        serde_json::json!({}),
        "session-1".to_owned(),
        12345,
    );
    mgr.request(
        "tool_b".to_owned(),
        serde_json::json!({}),
        "session-1".to_owned(),
        12345,
    );
    mgr.request(
        "tool_c".to_owned(),
        serde_json::json!({}),
        "session-2".to_owned(),
        12345,
    );

    assert_eq!(mgr.pending_count("session-1"), 2);
    assert_eq!(mgr.pending_count("session-2"), 1);
    assert_eq!(mgr.pending_count("session-3"), 0);
}
