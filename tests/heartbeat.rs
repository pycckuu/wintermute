//! Integration tests for `src/heartbeat/`.

#[path = "heartbeat/backup_test.rs"]
mod backup_test;
#[path = "heartbeat/digest_test.rs"]
mod digest_test;
#[path = "heartbeat/health_test.rs"]
mod health_test;
#[path = "heartbeat/proactive_test.rs"]
mod proactive_test;
#[path = "heartbeat/scheduler_test.rs"]
mod scheduler_test;
#[path = "heartbeat/tool_review_test.rs"]
mod tool_review_test;
