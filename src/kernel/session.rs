//! Session working memory and conversation history (spec 9.1, 9.2, 9.3).
//!
//! Provides per-principal session state including structured tool outputs
//! from recent tasks and sliding-window conversation history.
//!
//! Each principal maps to an isolated session namespace (Invariant A).
//! Phase 2 uses in-memory storage; Phase 3 moves to SQLCipher.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::{Principal, SecurityLabel};

/// Structured output from a single tool in a task (spec 9.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredToolOutput {
    /// Tool module name.
    pub tool: String,
    /// Action that was invoked.
    pub action: String,
    /// Structured output data (typed fields, not raw).
    pub output: serde_json::Value,
    /// Security label of this output.
    pub label: SecurityLabel,
}

/// Result of a completed task, stored in working memory (spec 9.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    /// Unique task identifier.
    pub task_id: Uuid,
    /// When the task completed.
    pub timestamp: DateTime<Utc>,
    /// Short summary of what was requested (not raw content).
    pub request_summary: String,
    /// Structured outputs from tools used in this task.
    pub tool_outputs: Vec<StructuredToolOutput>,
    /// Short summary of the response sent.
    pub response_summary: String,
    /// Highest security label of data touched.
    pub label: SecurityLabel,
}

/// A single conversation turn (spec 9.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    /// "user" or "assistant".
    pub role: String,
    /// Summary of the message.
    pub summary: String,
    /// When this turn occurred.
    pub timestamp: DateTime<Utc>,
}

/// Default capacity for recent task results.
const DEFAULT_RESULTS_CAPACITY: usize = 10;

/// Default capacity for conversation history turns.
const DEFAULT_HISTORY_CAPACITY: usize = 20;

/// Per-principal session working memory (spec 9.1).
///
/// Stores recent task results and conversation history in sliding
/// windows. Oldest entries are evicted when capacity is reached.
pub struct SessionWorkingMemory {
    recent_results: VecDeque<TaskResult>,
    conversation_history: VecDeque<ConversationTurn>,
    capacity_results: usize,
    capacity_history: usize,
}

impl Default for SessionWorkingMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionWorkingMemory {
    /// Create new empty session with default capacities (spec 9.1).
    ///
    /// Results capacity: 10, history capacity: 20.
    pub fn new() -> Self {
        Self {
            recent_results: VecDeque::new(),
            conversation_history: VecDeque::new(),
            capacity_results: DEFAULT_RESULTS_CAPACITY,
            capacity_history: DEFAULT_HISTORY_CAPACITY,
        }
    }

    /// Push a task result. Evicts oldest if at capacity (spec 9.1).
    pub fn push_result(&mut self, result: TaskResult) {
        if self.recent_results.len() >= self.capacity_results {
            self.recent_results.pop_front();
        }
        self.recent_results.push_back(result);
    }

    /// Push a conversation turn. Evicts oldest if at capacity (spec 9.2).
    pub fn push_turn(&mut self, turn: ConversationTurn) {
        if self.conversation_history.len() >= self.capacity_history {
            self.conversation_history.pop_front();
        }
        self.conversation_history.push_back(turn);
    }

    /// Recent task results for planner context (spec 9.3).
    pub fn recent_results(&self) -> &VecDeque<TaskResult> {
        &self.recent_results
    }

    /// Conversation history for planner/synthesizer context (spec 9.3).
    pub fn conversation_history(&self) -> &VecDeque<ConversationTurn> {
        &self.conversation_history
    }
}

/// Session store managing per-principal sessions (spec 9).
///
/// Phase 2 uses in-memory storage. Phase 3 moves to SQLCipher.
/// Each principal maps to an isolated session namespace (Invariant A).
pub struct SessionStore {
    sessions: HashMap<Principal, SessionWorkingMemory>,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStore {
    /// Create a new empty session store.
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    /// Get or create a session for a principal (spec 4.2).
    ///
    /// Creates a new empty session on first access for a given principal.
    pub fn get_or_create(&mut self, principal: &Principal) -> &mut SessionWorkingMemory {
        self.sessions.entry(principal.clone()).or_default()
    }

    /// Get a session for a principal (read-only, may return None).
    pub fn get(&self, principal: &Principal) -> Option<&SessionWorkingMemory> {
        self.sessions.get(principal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_task_result(summary: &str) -> TaskResult {
        TaskResult {
            task_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            request_summary: summary.to_owned(),
            tool_outputs: vec![StructuredToolOutput {
                tool: "email".to_owned(),
                action: "list".to_owned(),
                output: serde_json::json!({"count": 5}),
                label: SecurityLabel::Sensitive,
            }],
            response_summary: format!("Response to: {summary}"),
            label: SecurityLabel::Sensitive,
        }
    }

    fn make_turn(role: &str, summary: &str) -> ConversationTurn {
        ConversationTurn {
            role: role.to_owned(),
            summary: summary.to_owned(),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_push_result_and_retrieve() {
        let mut session = SessionWorkingMemory::new();
        let result = make_task_result("check email");
        session.push_result(result);

        assert_eq!(session.recent_results().len(), 1);
        assert_eq!(session.recent_results()[0].request_summary, "check email");
    }

    #[test]
    fn test_result_capacity_eviction() {
        let mut session = SessionWorkingMemory::new();

        // Push 11 results — capacity is 10, oldest should be evicted
        for i in 0..11 {
            session.push_result(make_task_result(&format!("task {i}")));
        }

        assert_eq!(session.recent_results().len(), 10);
        // First entry should be "task 1" (task 0 was evicted)
        assert_eq!(session.recent_results()[0].request_summary, "task 1");
        // Last entry should be "task 10"
        assert_eq!(session.recent_results()[9].request_summary, "task 10");
    }

    #[test]
    fn test_push_turn_and_retrieve() {
        let mut session = SessionWorkingMemory::new();
        let turn = make_turn("user", "What meetings do I have?");
        session.push_turn(turn);

        assert_eq!(session.conversation_history().len(), 1);
        assert_eq!(session.conversation_history()[0].role, "user");
        assert_eq!(
            session.conversation_history()[0].summary,
            "What meetings do I have?"
        );
    }

    #[test]
    fn test_turn_capacity_eviction() {
        let mut session = SessionWorkingMemory::new();

        // Push 21 turns — capacity is 20, oldest should be evicted
        for i in 0..21 {
            session.push_turn(make_turn("user", &format!("turn {i}")));
        }

        assert_eq!(session.conversation_history().len(), 20);
        // First entry should be "turn 1" (turn 0 was evicted)
        assert_eq!(session.conversation_history()[0].summary, "turn 1");
        // Last entry should be "turn 20"
        assert_eq!(session.conversation_history()[19].summary, "turn 20");
    }

    #[test]
    fn test_session_store_isolation() {
        // Regression test 1: two principals get separate sessions,
        // data doesn't leak between them (Invariant A).
        let mut store = SessionStore::new();

        let owner = Principal::Owner;
        let peer = Principal::TelegramPeer("12345".to_owned());

        // Push data to owner's session
        store
            .get_or_create(&owner)
            .push_result(make_task_result("owner task"));
        store
            .get_or_create(&owner)
            .push_turn(make_turn("user", "owner message"));

        // Push data to peer's session
        store
            .get_or_create(&peer)
            .push_result(make_task_result("peer task"));

        // Verify isolation: owner sees only their data
        let owner_session = store.get(&owner).expect("owner session should exist");
        assert_eq!(owner_session.recent_results().len(), 1);
        assert_eq!(
            owner_session.recent_results()[0].request_summary,
            "owner task"
        );
        assert_eq!(owner_session.conversation_history().len(), 1);

        // Verify isolation: peer sees only their data
        let peer_session = store.get(&peer).expect("peer session should exist");
        assert_eq!(peer_session.recent_results().len(), 1);
        assert_eq!(
            peer_session.recent_results()[0].request_summary,
            "peer task"
        );
        assert_eq!(peer_session.conversation_history().len(), 0);
    }

    #[test]
    fn test_session_store_get_nonexistent() {
        let store = SessionStore::new();
        let principal = Principal::TelegramPeer("99999".to_owned());
        assert!(
            store.get(&principal).is_none(),
            "should return None for unknown principal"
        );
    }

    #[test]
    fn test_session_store_get_or_create() {
        let mut store = SessionStore::new();
        let principal = Principal::Owner;

        // First call creates the session
        store
            .get_or_create(&principal)
            .push_result(make_task_result("first"));

        // Second call returns the existing session
        let session = store.get_or_create(&principal);
        assert_eq!(session.recent_results().len(), 1);
        assert_eq!(session.recent_results()[0].request_summary, "first");
    }

    #[test]
    fn test_task_result_serialization() {
        let result = TaskResult {
            task_id: Uuid::nil(),
            timestamp: Utc::now(),
            request_summary: "check email".to_owned(),
            tool_outputs: vec![StructuredToolOutput {
                tool: "email".to_owned(),
                action: "list".to_owned(),
                output: serde_json::json!({"emails": [{"id": "msg_1", "subject": "Hello"}]}),
                label: SecurityLabel::Sensitive,
            }],
            response_summary: "Listed 1 email".to_owned(),
            label: SecurityLabel::Sensitive,
        };

        let json = serde_json::to_string(&result).expect("should serialize");
        let deserialized: TaskResult = serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(deserialized.task_id, Uuid::nil());
        assert_eq!(deserialized.request_summary, "check email");
        assert_eq!(deserialized.tool_outputs.len(), 1);
        assert_eq!(deserialized.tool_outputs[0].tool, "email");
        assert_eq!(deserialized.label, SecurityLabel::Sensitive);
    }

    #[test]
    fn test_empty_session_defaults() {
        let session = SessionWorkingMemory::new();
        assert!(session.recent_results().is_empty());
        assert!(session.conversation_history().is_empty());
    }

    #[test]
    fn test_conversation_turn_serialization() {
        let turn = ConversationTurn {
            role: "assistant".to_owned(),
            summary: "Listed 3 emails".to_owned(),
            timestamp: Utc::now(),
        };

        let json = serde_json::to_string(&turn).expect("should serialize");
        let deserialized: ConversationTurn =
            serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(deserialized.role, "assistant");
        assert_eq!(deserialized.summary, "Listed 3 emails");
    }

    #[test]
    fn test_structured_tool_output_serialization() {
        let output = StructuredToolOutput {
            tool: "calendar".to_owned(),
            action: "freebusy".to_owned(),
            output: serde_json::json!({"free": true, "date": "2026-03-15"}),
            label: SecurityLabel::Internal,
        };

        let json = serde_json::to_string(&output).expect("should serialize");
        let deserialized: StructuredToolOutput =
            serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(deserialized.tool, "calendar");
        assert_eq!(deserialized.action, "freebusy");
        assert_eq!(deserialized.label, SecurityLabel::Internal);
    }
}
