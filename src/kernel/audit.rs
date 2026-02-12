/// Audit logger for privileged operations (spec 6.7).
///
/// Writes structured JSON entries, one per line, to an append-only sink.
/// Secrets are never logged; tool arguments are included but secret values
/// are redacted before reaching the audit logger.
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use chrono::Utc;
use serde::Serialize;
use uuid::Uuid;

use crate::types::{CapabilityToken, SecurityLabel, Task};

/// Audit event type discriminator (spec 6.7).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    /// A new task was created from a template.
    TaskCreated,
    /// A tool was invoked via capability token.
    ToolInvoked,
    /// A human approval decision was recorded.
    ApprovalDecision,
    /// A policy violation was detected.
    PolicyViolation,
    /// Data was sent to an output sink.
    Egress,
    /// An error occurred during processing.
    Error,
}

/// A single structured audit log entry (spec 6.7).
#[derive(Debug, Serialize)]
struct AuditEntry {
    timestamp: String,
    trace_id: String,
    event_type: AuditEventType,
    details: serde_json::Value,
}

/// Audit logger writing structured JSON to an append-only sink (spec 6.7).
pub struct AuditLogger {
    writer: Mutex<Box<dyn Write + Send>>,
}

impl AuditLogger {
    /// Create an audit logger that appends to the given file path.
    pub fn new(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.as_ref())?;
        Ok(Self {
            writer: Mutex::new(Box::new(file)),
        })
    }

    /// Create an audit logger from an arbitrary writer (for testing).
    pub fn from_writer(writer: Box<dyn Write + Send>) -> Self {
        Self {
            writer: Mutex::new(writer),
        }
    }

    /// Log task creation (spec 6.7).
    pub fn log_task_created(&self, task: &Task) -> anyhow::Result<()> {
        self.write_entry(
            AuditEventType::TaskCreated,
            &task.trace_id,
            serde_json::json!({
                "task_id": task.task_id,
                "template_id": task.template_id,
                "principal": task.principal,
                "trigger_event": task.trigger_event,
            }),
        )
    }

    /// Log tool invocation (spec 6.7).
    pub fn log_tool_invoked(
        &self,
        cap: &CapabilityToken,
        args: &serde_json::Value,
    ) -> anyhow::Result<()> {
        self.write_entry(
            AuditEventType::ToolInvoked,
            &cap.task_id.to_string(),
            serde_json::json!({
                "capability_id": cap.capability_id,
                "tool": cap.tool,
                "resource_scope": cap.resource_scope,
                "args": args,
            }),
        )
    }

    /// Log approval decision (spec 6.7).
    pub fn log_approval(&self, task_id: Uuid, approved: bool, reason: &str) -> anyhow::Result<()> {
        self.write_entry(
            AuditEventType::ApprovalDecision,
            &task_id.to_string(),
            serde_json::json!({
                "task_id": task_id,
                "approved": approved,
                "reason": reason,
            }),
        )
    }

    /// Log policy violation (spec 6.7).
    pub fn log_violation(&self, description: &str) -> anyhow::Result<()> {
        self.write_entry(
            AuditEventType::PolicyViolation,
            "",
            serde_json::json!({
                "description": description,
            }),
        )
    }

    /// Log egress event (spec 6.7).
    pub fn log_egress(&self, sink: &str, label: SecurityLabel, size: usize) -> anyhow::Result<()> {
        self.write_entry(
            AuditEventType::Egress,
            "",
            serde_json::json!({
                "sink": sink,
                "label": label,
                "size": size,
            }),
        )
    }

    /// Write a single JSON line to the audit log.
    fn write_entry(
        &self,
        event_type: AuditEventType,
        trace_id: &str,
        details: serde_json::Value,
    ) -> anyhow::Result<()> {
        let entry = AuditEntry {
            timestamp: Utc::now().to_rfc3339(),
            trace_id: trace_id.to_owned(),
            event_type,
            details,
        };
        let line = serde_json::to_string(&entry)?;
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("audit lock poisoned: {e}"))?;
        writeln!(writer, "{line}")?;
        writer.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Principal, TaintLevel, TaintSet, TaskState};
    use std::io::Cursor;
    use std::sync::Arc;

    /// Shared buffer for capturing audit output in tests.
    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Cursor<Vec<u8>>>>);

    impl SharedBuf {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Cursor::new(Vec::new()))))
        }

        fn contents(&self) -> String {
            let cursor = self.0.lock().expect("test lock");
            String::from_utf8_lossy(cursor.get_ref()).to_string()
        }
    }

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("test lock").write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.0.lock().expect("test lock").flush()
        }
    }

    fn test_task() -> Task {
        Task {
            task_id: Uuid::nil(),
            template_id: "test_template".to_owned(),
            principal: Principal::Owner,
            trigger_event: Uuid::nil(),
            data_ceiling: SecurityLabel::Sensitive,
            allowed_tools: vec!["email.list".to_owned()],
            denied_tools: vec![],
            max_tool_calls: 10,
            output_sinks: vec!["sink:telegram:owner".to_owned()],
            trace_id: "trace-001".to_owned(),
            state: TaskState::Extracting,
        }
    }

    #[test]
    fn test_log_task_created() {
        let buf = SharedBuf::new();
        let logger = AuditLogger::from_writer(Box::new(buf.clone()));

        logger.log_task_created(&test_task()).expect("should log");

        let output = buf.contents();
        let entry: serde_json::Value = serde_json::from_str(output.trim()).expect("valid JSON");
        assert_eq!(entry["event_type"], "task_created");
        assert_eq!(entry["trace_id"], "trace-001");
        assert_eq!(entry["details"]["template_id"], "test_template");
    }

    #[test]
    fn test_log_tool_invoked() {
        let buf = SharedBuf::new();
        let logger = AuditLogger::from_writer(Box::new(buf.clone()));

        let cap = CapabilityToken {
            capability_id: Uuid::nil(),
            task_id: Uuid::nil(),
            template_id: "test".to_owned(),
            principal: Principal::Owner,
            tool: "email.list".to_owned(),
            resource_scope: "account:personal".to_owned(),
            taint_of_arguments: TaintSet {
                level: TaintLevel::Clean,
                origin: "owner".to_owned(),
                touched_by: vec![],
            },
            issued_at: Utc::now(),
            expires_at: Utc::now(),
            max_invocations: 1,
        };

        logger
            .log_tool_invoked(&cap, &serde_json::json!({"limit": 10}))
            .expect("should log");

        let output = buf.contents();
        let entry: serde_json::Value = serde_json::from_str(output.trim()).expect("valid JSON");
        assert_eq!(entry["event_type"], "tool_invoked");
        assert_eq!(entry["details"]["tool"], "email.list");
    }

    #[test]
    fn test_log_egress() {
        let buf = SharedBuf::new();
        let logger = AuditLogger::from_writer(Box::new(buf.clone()));

        logger
            .log_egress("sink:telegram:owner", SecurityLabel::Sensitive, 256)
            .expect("should log");

        let output = buf.contents();
        let entry: serde_json::Value = serde_json::from_str(output.trim()).expect("valid JSON");
        assert_eq!(entry["event_type"], "egress");
        assert_eq!(entry["details"]["sink"], "sink:telegram:owner");
        assert_eq!(entry["details"]["size"], 256);
    }

    #[test]
    fn test_multiple_entries() {
        let buf = SharedBuf::new();
        let logger = AuditLogger::from_writer(Box::new(buf.clone()));

        logger.log_task_created(&test_task()).expect("log 1");
        logger.log_violation("test violation").expect("log 2");
        logger
            .log_egress("sink:test", SecurityLabel::Public, 100)
            .expect("log 3");

        let output = buf.contents();
        let lines: Vec<&str> = output.trim().lines().collect();
        assert_eq!(lines.len(), 3);

        // Each line must be valid JSON
        for line in &lines {
            serde_json::from_str::<serde_json::Value>(line)
                .expect("each line should be valid JSON");
        }
    }
}
