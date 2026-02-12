//! Approval Queue -- human-in-the-loop for tainted writes (spec 6.6, 4.4).
//!
//! Manages pending approval requests with timeout support.
//! When the plan executor encounters a write that requires human approval
//! (per graduated taint rules), it submits a request to the queue and
//! receives a `tokio::sync::oneshot` receiver to await the decision.
//!
//! The kernel resolves requests when the owner clicks Approve/Deny
//! (e.g. via Telegram inline buttons). Requests that exceed their
//! timeout are auto-denied by the periodic `cleanup_expired` sweep.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use thiserror::Error;
use tokio::sync::oneshot;
use tracing::{info, warn};
use uuid::Uuid;

use crate::types::TaintLevel;

/// Default approval timeout in seconds (spec 18.1: `approval_timeout_seconds = 300`).
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Same value as i64 for chrono interop (300 fits in i64 trivially).
const DEFAULT_TIMEOUT_SECS_I64: i64 = 300;

/// An approval request pending human decision (spec 6.6).
///
/// Contains all context needed for the owner to make an informed
/// approve/deny decision: what action, what data (redacted preview),
/// what taint level, and what output sink.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// Unique ID for this approval request.
    pub approval_id: Uuid,
    /// Task that requires approval.
    pub task_id: Uuid,
    /// Plan step index that triggered the approval.
    pub step: usize,
    /// Description of the action needing approval.
    pub action_description: String,
    /// Redacted preview of the data involved.
    pub data_preview: String,
    /// Taint level of the data.
    pub taint_level: TaintLevel,
    /// Target output sink.
    pub sink: String,
    /// Human-readable reason approval is required.
    pub reason: String,
    /// When the request was created.
    pub created_at: DateTime<Utc>,
    /// How long to wait before auto-denying.
    pub timeout: Duration,
}

/// Result of an approval decision (spec 6.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalResult {
    /// Owner approved the action.
    Approved,
    /// Owner denied the action.
    Denied,
    /// Request exceeded its timeout without a decision.
    TimedOut,
}

/// Approval queue errors.
#[derive(Debug, Error)]
pub enum ApprovalError {
    /// No pending request with this ID.
    #[error("approval request not found: {0}")]
    NotFound(Uuid),
    /// The request was already resolved (approved, denied, or timed out).
    #[error("approval request already resolved: {0}")]
    AlreadyResolved(Uuid),
}

/// Approval Queue managing pending human-approval requests (spec 6.6).
///
/// The queue stores pending requests alongside oneshot senders.
/// When a request is resolved (by owner action or timeout), the
/// result is sent through the channel, unblocking the executor.
pub struct ApprovalQueue {
    /// Pending requests keyed by approval_id, paired with their oneshot senders.
    pending: HashMap<Uuid, PendingEntry>,
    /// Default timeout for new requests (spec 18.1: 300s).
    default_timeout: Duration,
}

/// Internal entry pairing a request with its notification channel.
struct PendingEntry {
    request: ApprovalRequest,
    sender: oneshot::Sender<ApprovalResult>,
}

// Manual Debug impl because oneshot::Sender doesn't implement Debug.
impl std::fmt::Debug for PendingEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingEntry")
            .field("request", &self.request)
            .field("sender", &"<oneshot::Sender>")
            .finish()
    }
}

impl ApprovalQueue {
    /// Create a new approval queue with the given default timeout (spec 6.6).
    pub fn new(default_timeout: Duration) -> Self {
        Self {
            pending: HashMap::new(),
            default_timeout,
        }
    }

    /// Create an approval queue with the spec-default 300-second timeout.
    pub fn with_default_timeout() -> Self {
        Self::new(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
    }

    /// Submit a new approval request (spec 6.6).
    ///
    /// Returns a receiver that will yield the decision once the request
    /// is resolved (approved, denied, or timed out). The caller should
    /// `.await` this receiver to suspend the task until a decision arrives.
    pub fn submit(&mut self, request: ApprovalRequest) -> oneshot::Receiver<ApprovalResult> {
        let (tx, rx) = oneshot::channel();
        let id = request.approval_id;

        info!(
            approval_id = %id,
            task_id = %request.task_id,
            action = %request.action_description,
            taint = ?request.taint_level,
            sink = %request.sink,
            "approval request submitted"
        );

        self.pending.insert(
            id,
            PendingEntry {
                request,
                sender: tx,
            },
        );

        rx
    }

    /// Resolve a pending approval request (spec 6.6).
    ///
    /// Called when the owner clicks Approve/Deny (e.g. Telegram inline
    /// buttons). Sends the result through the oneshot channel, unblocking
    /// the waiting executor. If the receiver has already been dropped
    /// (task cancelled), the send is silently ignored.
    pub fn resolve(
        &mut self,
        approval_id: Uuid,
        result: ApprovalResult,
    ) -> Result<(), ApprovalError> {
        let entry = self
            .pending
            .remove(&approval_id)
            .ok_or(ApprovalError::NotFound(approval_id))?;

        info!(
            approval_id = %approval_id,
            task_id = %entry.request.task_id,
            result = ?result,
            action = %entry.request.action_description,
            "approval resolved"
        );

        // If the receiver is dropped (task cancelled), this is a no-op.
        let _send_result = entry.sender.send(result);

        Ok(())
    }

    /// Retrieve a pending request by ID (for rendering to the owner).
    pub fn get_pending(&self, approval_id: &Uuid) -> Option<&ApprovalRequest> {
        self.pending.get(approval_id).map(|e| &e.request)
    }

    /// Number of requests currently awaiting a decision.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// List all pending approval requests (for status display).
    pub fn list_pending(&self) -> Vec<&ApprovalRequest> {
        self.pending.values().map(|e| &e.request).collect()
    }

    /// Remove expired requests and auto-deny them as `TimedOut` (spec 6.6).
    ///
    /// Should be called periodically (e.g. every 10-30 seconds) by the
    /// kernel's main loop or a dedicated timer task. Each expired request
    /// receives `ApprovalResult::TimedOut` through its oneshot channel.
    pub fn cleanup_expired(&mut self) -> usize {
        let now = Utc::now();

        // Collect IDs of expired entries first to avoid borrow conflicts.
        let expired_ids: Vec<Uuid> = self
            .pending
            .iter()
            .filter(|(_, entry)| is_expired(&entry.request, now))
            .map(|(id, _)| *id)
            .collect();

        let count = expired_ids.len();

        for id in expired_ids {
            if let Some(entry) = self.pending.remove(&id) {
                warn!(
                    approval_id = %id,
                    task_id = %entry.request.task_id,
                    action = %entry.request.action_description,
                    "approval request timed out"
                );
                let _send_result = entry.sender.send(ApprovalResult::TimedOut);
            }
        }

        count
    }

    /// Default timeout for new requests.
    pub fn default_timeout(&self) -> Duration {
        self.default_timeout
    }
}

/// Check whether a request has exceeded its timeout.
///
/// Converts `std::time::Duration` to `chrono::TimeDelta` for comparison.
/// Falls back to the spec default (300s) if the duration overflows
/// (practically impossible but satisfies clippy).
fn is_expired(request: &ApprovalRequest, now: DateTime<Utc>) -> bool {
    let elapsed = now.signed_duration_since(request.created_at);
    let timeout_td = chrono::TimeDelta::from_std(request.timeout)
        .unwrap_or_else(|_| chrono::TimeDelta::seconds(DEFAULT_TIMEOUT_SECS_I64));
    elapsed > timeout_td
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a test approval request with sensible defaults.
    fn test_request() -> ApprovalRequest {
        ApprovalRequest {
            approval_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            step: 1,
            action_description: "email.send to user@example.com".to_string(),
            data_preview: "Re: Meeting notes...".to_string(),
            taint_level: TaintLevel::Raw,
            sink: "sink:email:personal".to_string(),
            reason: "raw external content requires approval for writes".to_string(),
            created_at: Utc::now(),
            timeout: Duration::from_secs(300),
        }
    }

    /// Helper: create an already-expired request (created 600s ago, timeout 300s).
    fn expired_request() -> ApprovalRequest {
        let mut req = test_request();
        req.created_at = Utc::now()
            .checked_sub_signed(chrono::TimeDelta::seconds(600))
            .expect("test: 600s subtraction should not overflow");
        req.timeout = Duration::from_secs(300);
        req
    }

    // ── Submit and resolve ──

    #[tokio::test]
    async fn test_submit_and_approve() {
        let mut queue = ApprovalQueue::new(Duration::from_secs(300));
        let request = test_request();
        let id = request.approval_id;

        let rx = queue.submit(request);
        assert_eq!(queue.pending_count(), 1);

        queue
            .resolve(id, ApprovalResult::Approved)
            .expect("should resolve");
        assert_eq!(queue.pending_count(), 0);

        let result = rx.await.expect("should receive result");
        assert_eq!(result, ApprovalResult::Approved);
    }

    #[tokio::test]
    async fn test_submit_and_deny() {
        let mut queue = ApprovalQueue::new(Duration::from_secs(300));
        let request = test_request();
        let id = request.approval_id;

        let rx = queue.submit(request);
        queue
            .resolve(id, ApprovalResult::Denied)
            .expect("should resolve");

        let result = rx.await.expect("should receive result");
        assert_eq!(result, ApprovalResult::Denied);
    }

    // ── Error cases ──

    #[test]
    fn test_resolve_not_found() {
        let mut queue = ApprovalQueue::new(Duration::from_secs(300));
        let result = queue.resolve(Uuid::new_v4(), ApprovalResult::Approved);
        assert!(result.is_err());
        assert!(
            matches!(result, Err(ApprovalError::NotFound(_))),
            "expected NotFound error"
        );
    }

    #[test]
    fn test_resolve_already_resolved() {
        let mut queue = ApprovalQueue::new(Duration::from_secs(300));
        let request = test_request();
        let id = request.approval_id;

        let _rx = queue.submit(request);
        queue
            .resolve(id, ApprovalResult::Approved)
            .expect("first resolve should succeed");

        // Second resolve should fail: entry was already removed.
        let result = queue.resolve(id, ApprovalResult::Denied);
        assert!(
            matches!(result, Err(ApprovalError::NotFound(_))),
            "second resolve should return NotFound (entry removed)"
        );
    }

    // ── Pending state queries ──

    #[test]
    fn test_pending_count() {
        let mut queue = ApprovalQueue::new(Duration::from_secs(300));
        assert_eq!(queue.pending_count(), 0);

        let _rx1 = queue.submit(test_request());
        assert_eq!(queue.pending_count(), 1);

        let _rx2 = queue.submit(test_request());
        assert_eq!(queue.pending_count(), 2);
    }

    #[test]
    fn test_get_pending() {
        let mut queue = ApprovalQueue::new(Duration::from_secs(300));
        let request = test_request();
        let id = request.approval_id;

        let _rx = queue.submit(request);

        let pending = queue.get_pending(&id);
        assert!(pending.is_some());
        assert_eq!(
            pending.expect("just verified Some").action_description,
            "email.send to user@example.com"
        );
    }

    #[test]
    fn test_get_pending_not_found() {
        let queue = ApprovalQueue::new(Duration::from_secs(300));
        assert!(queue.get_pending(&Uuid::new_v4()).is_none());
    }

    #[test]
    fn test_list_pending() {
        let mut queue = ApprovalQueue::new(Duration::from_secs(300));

        let _rx1 = queue.submit(test_request());
        let _rx2 = queue.submit(test_request());

        let listed = queue.list_pending();
        assert_eq!(listed.len(), 2);
    }

    // ── Timeout / expiry ──

    #[test]
    fn test_cleanup_expired_removes_old_requests() {
        let mut queue = ApprovalQueue::new(Duration::from_secs(300));

        let _rx = queue.submit(expired_request());
        assert_eq!(queue.pending_count(), 1);

        let cleaned = queue.cleanup_expired();
        assert_eq!(cleaned, 1);
        assert_eq!(queue.pending_count(), 0);
    }

    #[tokio::test]
    async fn test_cleanup_sends_timeout_result() {
        let mut queue = ApprovalQueue::new(Duration::from_secs(300));

        let rx = queue.submit(expired_request());

        let cleaned = queue.cleanup_expired();
        assert_eq!(cleaned, 1);

        let result = rx.await.expect("should receive timeout result");
        assert_eq!(result, ApprovalResult::TimedOut);
    }

    #[test]
    fn test_non_expired_not_cleaned() {
        let mut queue = ApprovalQueue::new(Duration::from_secs(300));

        // Created just now with 300s timeout -- not expired.
        let _rx = queue.submit(test_request());

        let cleaned = queue.cleanup_expired();
        assert_eq!(cleaned, 0);
        assert_eq!(queue.pending_count(), 1);
    }

    #[tokio::test]
    async fn test_mixed_expired_and_fresh() {
        let mut queue = ApprovalQueue::new(Duration::from_secs(300));

        let expired_rx = queue.submit(expired_request());
        let _fresh_rx = queue.submit(test_request());
        assert_eq!(queue.pending_count(), 2);

        let cleaned = queue.cleanup_expired();
        assert_eq!(cleaned, 1);
        assert_eq!(queue.pending_count(), 1, "only fresh request should remain");

        let result = expired_rx.await.expect("expired should get TimedOut");
        assert_eq!(result, ApprovalResult::TimedOut);
    }

    // ── Default constructor ──

    #[test]
    fn test_default_timeout() {
        let queue = ApprovalQueue::with_default_timeout();
        assert_eq!(queue.default_timeout(), Duration::from_secs(300));
    }

    // ── Dropped receiver ──

    #[test]
    fn test_resolve_after_receiver_dropped() {
        let mut queue = ApprovalQueue::new(Duration::from_secs(300));
        let request = test_request();
        let id = request.approval_id;

        let rx = queue.submit(request);
        drop(rx); // Simulate task cancellation.

        // Resolve should succeed even though receiver is gone.
        let result = queue.resolve(id, ApprovalResult::Approved);
        assert!(
            result.is_ok(),
            "resolve should not fail if receiver dropped"
        );
        assert_eq!(queue.pending_count(), 0);
    }
}
