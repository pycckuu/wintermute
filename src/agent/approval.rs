//! Non-blocking approval manager for tool calls requiring user confirmation.
//!
//! When the policy gate decides a tool call needs user approval, the
//! [`ApprovalManager`] stores the pending request and returns a short
//! base62 identifier. The user approves or denies via Telegram inline
//! keyboard callbacks containing that identifier.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use serde_json::Value;

/// Length of generated approval identifiers.
const APPROVAL_ID_LEN: usize = 8;

/// Base62 alphabet used for approval IDs.
const BASE62_CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Default approval expiry in minutes.
const APPROVAL_EXPIRY_MINUTES: i64 = 5;

/// A pending approval request waiting for user action.
#[derive(Debug, Clone)]
pub struct PendingApproval {
    /// Short base62 identifier.
    pub id: String,
    /// Name of the tool that requires approval.
    pub tool_name: String,
    /// Tool input arguments.
    pub tool_input: Value,
    /// Session that initiated the request.
    pub session_id: String,
    /// Telegram user ID that must approve.
    pub user_id: i64,
    /// When the request was created.
    pub created_at: DateTime<Utc>,
    /// When the request expires.
    pub expires_at: DateTime<Utc>,
}

/// Result of resolving an approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalResult {
    /// The tool call was approved.
    Approved {
        /// Session that initiated the request.
        session_id: String,
        /// Tool name.
        tool_name: String,
        /// Tool input (serialized to String for Eq).
        tool_input: String,
    },
    /// The tool call was denied.
    Denied {
        /// Session that initiated the request.
        session_id: String,
        /// Tool name.
        tool_name: String,
    },
    /// The approval request has expired.
    Expired,
    /// No approval request found with the given ID.
    NotFound,
    /// The resolving user does not match the request owner.
    WrongUser,
}

/// Manages pending tool-call approval requests.
///
/// Uses a sync [`Mutex`] since the critical section is brief (no awaits).
#[derive(Debug)]
pub struct ApprovalManager {
    pending: Mutex<HashMap<String, PendingApproval>>,
}

impl ApprovalManager {
    /// Create an empty approval manager.
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Submit a new approval request, returning the generated 8-char base62 ID.
    pub fn request(
        &self,
        tool_name: String,
        tool_input: Value,
        session_id: String,
        user_id: i64,
    ) -> String {
        let id = generate_base62_id();
        let now = Utc::now();
        let offset = Duration::minutes(APPROVAL_EXPIRY_MINUTES);
        let expires_at = now.checked_add_signed(offset).unwrap_or(now);

        let approval = PendingApproval {
            id: id.clone(),
            tool_name,
            tool_input,
            session_id,
            user_id,
            created_at: now,
            expires_at,
        };

        if let Ok(mut map) = self.pending.lock() {
            map.insert(id.clone(), approval);
        }

        id
    }

    /// Resolve an approval request by ID.
    ///
    /// The entry is removed on resolution regardless of outcome (single-use).
    pub fn resolve(&self, approval_id: &str, approved: bool, user_id: i64) -> ApprovalResult {
        let mut map = match self.pending.lock() {
            Ok(m) => m,
            Err(_) => return ApprovalResult::NotFound,
        };

        let entry = match map.remove(approval_id) {
            Some(e) => e,
            None => return ApprovalResult::NotFound,
        };

        if entry.user_id != user_id {
            // Put it back — wrong user should not consume the request.
            map.insert(approval_id.to_owned(), entry);
            return ApprovalResult::WrongUser;
        }

        if Utc::now() > entry.expires_at {
            return ApprovalResult::Expired;
        }

        if approved {
            ApprovalResult::Approved {
                session_id: entry.session_id,
                tool_name: entry.tool_name,
                tool_input: entry.tool_input.to_string(),
            }
        } else {
            ApprovalResult::Denied {
                session_id: entry.session_id,
                tool_name: entry.tool_name,
            }
        }
    }

    /// Remove all expired entries.
    pub fn gc_expired(&self) {
        if let Ok(mut map) = self.pending.lock() {
            let now = Utc::now();
            map.retain(|_, v| v.expires_at > now);
        }
    }

    /// Access the underlying pending map (for testing expiry manipulation).
    ///
    /// Returns a `MutexGuard` wrapped in `Result`.
    pub fn pending_map(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<String, PendingApproval>>, String> {
        self.pending
            .lock()
            .map_err(|e| format!("lock poisoned: {e}"))
    }

    /// Count pending approvals for a given session.
    pub fn pending_count(&self, session_id: &str) -> usize {
        match self.pending.lock() {
            Ok(map) => map.values().filter(|v| v.session_id == session_id).count(),
            Err(_) => 0,
        }
    }
}

impl Default for ApprovalManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Generate an 8-character base62 identifier.
fn generate_base62_id() -> String {
    let mut rng = rand::thread_rng();
    (0..APPROVAL_ID_LEN)
        .map(|_| {
            let idx = rng.gen_range(0..BASE62_CHARS.len());
            BASE62_CHARS[idx] as char
        })
        .collect()
}
