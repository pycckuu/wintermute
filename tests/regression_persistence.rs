#![allow(missing_docs)]
//! Persistence & recovery regression tests (feature spec: persistence-recovery, section 11).
//!
//! Tests R1–R12 validate task journaling, recovery classification,
//! adapter state persistence, graceful shutdown logic, and startup
//! notification correctness.
//!
//! All tests use in-memory SQLite journals (`TaskJournal::open_in_memory()`).

use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};
use uuid::Uuid;

use pfar::kernel::journal::{
    CompletedStep, CreateTaskParams, PendingApprovalRecord, PendingCredentialRecord,
    PersistedTaskState, TaskJournal,
};
use pfar::kernel::recovery::{
    determine_recovery_action, determine_step_recovery, recover_tasks, RecoveryAction,
    RecoveryReport, StepRecovery,
};
use pfar::types::SecurityLabel;

// ── Helpers ──

fn make_journal() -> TaskJournal {
    TaskJournal::open_in_memory().expect("in-memory journal")
}

fn simple_params(task_id: Uuid) -> CreateTaskParams {
    CreateTaskParams {
        task_id,
        template_id: "test_template".to_owned(),
        principal: "owner".to_owned(),
        trigger_event: None,
        data_ceiling: SecurityLabel::Sensitive,
        output_sinks: vec!["sink:telegram:owner".to_owned()],
        trace_id: Some("trace-regression".to_owned()),
    }
}

fn make_completed_step(step: usize, tool: &str, semantics: &str) -> CompletedStep {
    CompletedStep {
        step,
        tool: tool.to_owned(),
        action_semantics: semantics.to_owned(),
        result_json: serde_json::json!({"ok": true}),
        label: SecurityLabel::Sensitive,
        completed_at: Utc::now(),
    }
}

// ── R1: Task in Executing with 1 step done → recovery resumes from step 2 ──

#[test]
fn regression_r1_executing_with_step_resumes() {
    let j = make_journal();
    let id = Uuid::new_v4();
    j.create_task(&simple_params(id)).expect("create");

    // Advance to Executing state.
    j.update_state(
        id,
        &PersistedTaskState::Executing {
            current_step: 0,
            completed_steps: vec![],
        },
    )
    .expect("state");

    // Record 1 completed step.
    let step = make_completed_step(1, "email.list", "read");
    j.append_completed_step(id, &step).expect("append");

    // Recovery should resume (not retry from scratch).
    let task = j.get_task(id).expect("get");
    let action = determine_recovery_action(&task, ChronoDuration::minutes(10), Utc::now());
    assert_eq!(action, RecoveryAction::ResumeExecution);

    // Verify the completed step is available for resume.
    if let PersistedTaskState::Executing {
        completed_steps, ..
    } = &task.state
    {
        assert_eq!(completed_steps.len(), 1);
        assert_eq!(completed_steps[0].step, 1);
        assert_eq!(completed_steps[0].tool, "email.list");
    } else {
        panic!("expected Executing state");
    }
}

// ── R2: Task in AwaitingApproval → recovery reprompts ──

#[test]
fn regression_r2_awaiting_approval_reprompts() {
    let j = make_journal();
    let id = Uuid::new_v4();
    let approval_id = Uuid::new_v4();
    j.create_task(&simple_params(id)).expect("create");

    j.update_state(
        id,
        &PersistedTaskState::AwaitingApproval {
            approval_id,
            step: 2,
        },
    )
    .expect("state");

    // Persist the approval record.
    j.save_pending_approval(&PendingApprovalRecord {
        approval_id,
        task_id: id,
        action_type: "tainted_write".to_owned(),
        description: "Write to Notion".to_owned(),
        data_preview: Some("Summary of meeting".to_owned()),
        taint_level: Some("Extracted".to_owned()),
        target_sink: Some("sink:notion:page".to_owned()),
        tool: Some("notion.create_page".to_owned()),
        step: Some(2),
        created_at: Utc::now(),
        expires_at: Utc::now() + ChronoDuration::minutes(5),
        status: "pending".to_owned(),
    })
    .expect("save approval");

    // Recovery should reprompt.
    let task = j.get_task(id).expect("get");
    let action = determine_recovery_action(&task, ChronoDuration::minutes(10), Utc::now());
    assert_eq!(action, RecoveryAction::RepromptApproval);

    // Approval record should be loadable.
    let approval = j
        .load_pending_approval(approval_id)
        .expect("load approval")
        .expect("approval should exist");
    assert_eq!(approval.status, "pending");
    assert_eq!(approval.task_id, id);
}

// ── R3: Task in Planning → recovery retries from scratch ──

#[test]
fn regression_r3_planning_retries_from_scratch() {
    let j = make_journal();
    let id = Uuid::new_v4();
    j.create_task(&simple_params(id)).expect("create");
    j.update_state(id, &PersistedTaskState::Planning)
        .expect("state");

    let task = j.get_task(id).expect("get");
    let action = determine_recovery_action(&task, ChronoDuration::minutes(10), Utc::now());
    assert_eq!(action, RecoveryAction::RetryFromScratch);

    // No steps completed — no side effects to worry about.
    if let PersistedTaskState::Planning = &task.state {
        // Expected.
    } else {
        panic!("expected Planning state");
    }
}

// ── R4: Write step in-progress at crash → RequireOwnerConfirmation ──

#[test]
fn regression_r4_write_in_progress_requires_confirmation() {
    let step = CompletedStep {
        step: 2,
        tool: "notion.create_page".to_owned(),
        action_semantics: "write".to_owned(),
        result_json: serde_json::Value::Null, // No result — was in progress.
        label: SecurityLabel::Sensitive,
        completed_at: Utc::now(),
    };

    let recovery = determine_step_recovery(&step, true);
    assert!(matches!(
        recovery,
        StepRecovery::RequireOwnerConfirmation { .. }
    ));

    if let StepRecovery::RequireOwnerConfirmation { message } = recovery {
        assert!(message.contains("notion.create_page"));
        assert!(message.contains("interrupted"));
    }
}

// ── R5: 5-min-old task (recoverable) + 15-min-old (abandoned) → max age enforcement ──

#[test]
fn regression_r5_max_age_enforcement() {
    let j = make_journal();
    let max_age = ChronoDuration::minutes(10);

    // Task 1: young (recoverable).
    let id1 = Uuid::new_v4();
    j.create_task(&simple_params(id1)).expect("create");
    j.update_state(id1, &PersistedTaskState::Planning)
        .expect("state");

    // Task 2: old (should be abandoned).
    let id2 = Uuid::new_v4();
    j.create_task(&simple_params(id2)).expect("create");
    j.update_state(
        id2,
        &PersistedTaskState::Executing {
            current_step: 1,
            completed_steps: vec![],
        },
    )
    .expect("state");

    // Simulate "15 minutes later" by using a future timestamp.
    let now = Utc::now();
    let task1 = j.get_task(id1).expect("get");
    let task2 = j.get_task(id2).expect("get");

    // Task 1 is young → retry.
    let action1 = determine_recovery_action(&task1, max_age, now);
    assert_eq!(action1, RecoveryAction::RetryFromScratch);

    // Task 2 evaluated 15 minutes in the future → abandon.
    let future = now + ChronoDuration::minutes(15);
    let action2 = determine_recovery_action(&task2, max_age, future);
    assert_eq!(action2, RecoveryAction::Abandon);

    // Run full recovery with a 0-second max_age to force abandonment of all tasks.
    let report = recover_tasks(&j, ChronoDuration::seconds(0)).expect("recover");
    assert_eq!(report.abandoned.len(), 2);
}

// ── R6: Telegram offset save/load roundtrip ──

#[test]
fn regression_r6_telegram_offset_roundtrip() {
    let j = make_journal();

    // No state initially.
    assert!(j.load_adapter_state("telegram").expect("load").is_none());

    // Save offset.
    let offset_json = serde_json::json!({"last_offset": 987654}).to_string();
    j.save_adapter_state("telegram", &offset_json)
        .expect("save");

    // Load and verify.
    let loaded = j
        .load_adapter_state("telegram")
        .expect("load")
        .expect("should have state");
    let parsed: serde_json::Value = serde_json::from_str(&loaded).expect("parse");
    assert_eq!(parsed["last_offset"], 987654);

    // Update offset (simulating next batch).
    let updated_json = serde_json::json!({"last_offset": 987660}).to_string();
    j.save_adapter_state("telegram", &updated_json)
        .expect("update");

    let reloaded = j
        .load_adapter_state("telegram")
        .expect("load")
        .expect("should have state");
    let reparsed: serde_json::Value = serde_json::from_str(&reloaded).expect("parse");
    assert_eq!(reparsed["last_offset"], 987660);

    // Other adapters are independent.
    assert!(j.load_adapter_state("slack").expect("load").is_none());
}

// ── R7: Placeholder (requires container manager from Phase 3) ──

#[test]
fn regression_r7_container_reconciliation_placeholder() {
    // Container reconciliation requires the container manager (Phase 3).
    // For now, verify that the recovery report can track orphan containers.
    let report = RecoveryReport {
        retried: vec![],
        resumed: vec![],
        reprompted: vec![],
        abandoned: vec![],
        orphan_containers: 5,
    };
    assert!(!report.is_clean());
    let msg = report.format_message();
    assert!(msg.contains("5 orphaned container(s) cleaned up"));
}

// ── R8: Shutdown waits for in-flight task ──
// Note: Full integration test with tokio::signal is not practical in unit tests.
// We validate the audit logger's SystemShutdown event and the recovery report
// correctness for the shutdown path.

#[test]
fn regression_r8_shutdown_logs_pending_tasks() {
    use pfar::kernel::audit::AuditLogger;
    use std::io::{Cursor, Write};
    use std::sync::Mutex;

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

    let buf = SharedBuf::new();
    let logger = AuditLogger::from_writer(Box::new(buf.clone()));

    // Simulate shutdown with 2 pending tasks.
    logger.log_system_shutdown(2).expect("log shutdown");

    let output = buf.contents();
    let entry: serde_json::Value = serde_json::from_str(output.trim()).expect("valid JSON");
    assert_eq!(entry["event_type"], "system_shutdown");
    assert_eq!(entry["details"]["pending_tasks"], 2);
}

// ── R9: Shutdown timeout → task stays in journal for recovery ──

#[test]
fn regression_r9_task_survives_shutdown_for_recovery() {
    let j = make_journal();
    let id = Uuid::new_v4();
    j.create_task(&simple_params(id)).expect("create");

    // Simulate task mid-execution at shutdown timeout.
    j.update_state(
        id,
        &PersistedTaskState::Executing {
            current_step: 1,
            completed_steps: vec![],
        },
    )
    .expect("state");
    let step = make_completed_step(1, "email.list", "read");
    j.append_completed_step(id, &step).expect("append");

    // Task stays in journal (not completed/failed) — simulating timeout.
    let task = j.get_task(id).expect("get");
    assert!(matches!(task.state, PersistedTaskState::Executing { .. }));

    // On next startup, recovery picks it up.
    let report = recover_tasks(&j, ChronoDuration::minutes(10)).expect("recover");
    assert_eq!(report.resumed.len(), 1);
    assert!(report.resumed.contains(&id));
}

// ── R10: Task in AwaitingCredential → recovery reprompts ──

#[test]
fn regression_r10_awaiting_credential_reprompts() {
    let j = make_journal();
    let id = Uuid::new_v4();
    let prompt_id = Uuid::new_v4();
    j.create_task(&simple_params(id)).expect("create");

    j.update_state(
        id,
        &PersistedTaskState::AwaitingCredential {
            service: "notion".to_owned(),
            prompt_message_id: Some("msg_42".to_owned()),
        },
    )
    .expect("state");

    // Persist the credential prompt.
    j.save_pending_credential(&PendingCredentialRecord {
        prompt_id,
        task_id: id,
        service: "notion".to_owned(),
        credential_type: "integration_token".to_owned(),
        instructions: "Go to notion.so/my-integrations...".to_owned(),
        vault_ref: "vault:notion_token".to_owned(),
        message_id: Some("msg_42".to_owned()),
        created_at: Utc::now(),
        expires_at: Utc::now() + ChronoDuration::minutes(10),
        status: "pending".to_owned(),
    })
    .expect("save credential");

    // Recovery should reprompt.
    let report = recover_tasks(&j, ChronoDuration::minutes(10)).expect("recover");
    assert_eq!(report.reprompted.len(), 1);
    assert!(report.reprompted.contains(&id));

    // Credential record should be loadable.
    let cred = j
        .load_pending_credential(prompt_id)
        .expect("load credential")
        .expect("credential should exist");
    assert_eq!(cred.service, "notion");
    assert_eq!(cred.status, "pending");
}

// ── R11: Multiple tasks in mixed states → correct report ──

#[test]
fn regression_r11_mixed_states_correct_report() {
    let j = make_journal();

    // Task 1: Extracting → retry.
    let id1 = Uuid::new_v4();
    j.create_task(&simple_params(id1)).expect("create");

    // Task 2: Executing with steps → resume.
    let id2 = Uuid::new_v4();
    j.create_task(&simple_params(id2)).expect("create");
    j.update_state(
        id2,
        &PersistedTaskState::Executing {
            current_step: 0,
            completed_steps: vec![],
        },
    )
    .expect("state");
    let step = make_completed_step(1, "calendar.freebusy", "read");
    j.append_completed_step(id2, &step).expect("append");

    // Task 3: Synthesizing → resume (resynthesize).
    let id3 = Uuid::new_v4();
    j.create_task(&simple_params(id3)).expect("create");
    j.update_state(id3, &PersistedTaskState::Synthesizing)
        .expect("state");

    // Task 4: AwaitingApproval → reprompt.
    let id4 = Uuid::new_v4();
    j.create_task(&simple_params(id4)).expect("create");
    j.update_state(
        id4,
        &PersistedTaskState::AwaitingApproval {
            approval_id: Uuid::new_v4(),
            step: 1,
        },
    )
    .expect("state");

    // Task 5: Completed → should NOT appear in recovery.
    let id5 = Uuid::new_v4();
    j.create_task(&simple_params(id5)).expect("create");
    j.mark_completed(id5).expect("complete");

    let report = recover_tasks(&j, ChronoDuration::minutes(10)).expect("recover");
    assert_eq!(report.retried.len(), 1); // id1 (Extracting)
    assert_eq!(report.resumed.len(), 2); // id2 (ResumeExecution) + id3 (Resynthesize)
    assert_eq!(report.reprompted.len(), 1); // id4
    assert!(report.abandoned.is_empty());
    assert!(!report.is_clean());

    // Verify message contains all categories.
    let msg = report.format_message();
    assert!(msg.contains("1 task(s) retried"));
    assert!(msg.contains("2 task(s) resumed"));
    assert!(msg.contains("1 approval/credential"));
}

// ── R12: Clean restart, no pending tasks → is_clean() true ──

#[test]
fn regression_r12_clean_restart() {
    let j = make_journal();

    // Only completed and failed tasks.
    let id1 = Uuid::new_v4();
    j.create_task(&simple_params(id1)).expect("create");
    j.mark_completed(id1).expect("complete");

    let id2 = Uuid::new_v4();
    j.create_task(&simple_params(id2)).expect("create");
    j.mark_failed(id2, "test error").expect("fail");

    let report = recover_tasks(&j, ChronoDuration::minutes(10)).expect("recover");
    assert!(report.is_clean());

    let msg = report.format_message();
    assert!(msg.contains("No pending tasks to recover"));
}
