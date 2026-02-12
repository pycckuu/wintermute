//! Recovery logic for crash recovery (feature spec: persistence-recovery, section 7).
//!
//! Classifies incomplete tasks from the journal and determines the
//! appropriate recovery action for each. Marks stale tasks as abandoned
//! and produces a report for owner notification.
//!
//! Actual re-execution of recovered tasks (feeding back through pipeline)
//! is deferred to a future implementation.

use chrono::{DateTime, Duration, Utc};
use tracing::{info, warn};
use uuid::Uuid;

use crate::kernel::journal::{
    CompletedStep, JournalError, PersistedTask, PersistedTaskState, TaskJournal,
};

/// What to do with a recovered task (feature spec: persistence-recovery 7.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Task was in Extract or Plan phase — no side effects yet, safe to retry.
    RetryFromScratch,
    /// Task was mid-Execute with completed steps — resume from next step.
    ResumeExecution,
    /// All tool steps done but synthesis didn't finish — re-run Phase 3.
    Resynthesize,
    /// Was waiting for human approval — re-send the approval message.
    RepromptApproval,
    /// Was waiting for credential input — re-send the prompt.
    RepromptCredential,
    /// Task is unrecoverable or too old — mark abandoned.
    Abandon,
}

/// How to handle a specific step during execution resume (feature spec: persistence-recovery 7.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepRecovery {
    /// Step already completed with result in journal — skip and use cached result.
    SkipWithCachedResult,
    /// Safe to retry (reads, or writes that hadn't started).
    Retry,
    /// Execute normally (write that hadn't started yet).
    Execute,
    /// Write was in progress at crash — ask owner whether to retry.
    RequireOwnerConfirmation {
        /// Message to show the owner.
        message: String,
    },
}

/// Summary of recovery actions taken at startup (feature spec: persistence-recovery 7.4).
#[derive(Debug, Clone, Default)]
pub struct RecoveryReport {
    /// Tasks retried from scratch (Extract/Plan phase).
    pub retried: Vec<Uuid>,
    /// Tasks resumed mid-execution or re-synthesized.
    pub resumed: Vec<Uuid>,
    /// Tasks with approvals/credentials re-prompted.
    pub reprompted: Vec<Uuid>,
    /// Tasks abandoned (too old or unrecoverable).
    pub abandoned: Vec<Uuid>,
    /// Orphaned containers killed (placeholder for Phase 3).
    pub orphan_containers: usize,
}

impl RecoveryReport {
    /// Format a human-readable recovery message for the owner (feature spec: persistence-recovery 7.4).
    pub fn format_message(&self) -> String {
        if self.is_clean() {
            return "System restarted. No pending tasks to recover.".to_owned();
        }

        let mut parts = vec!["System restarted. Recovery report:".to_owned()];

        if !self.retried.is_empty() {
            parts.push(format!(
                "- {} task(s) retried from scratch",
                self.retried.len()
            ));
        }
        if !self.resumed.is_empty() {
            parts.push(format!("- {} task(s) resumed", self.resumed.len()));
        }
        if !self.reprompted.is_empty() {
            parts.push(format!(
                "- {} approval/credential prompt(s) re-sent",
                self.reprompted.len()
            ));
        }
        if !self.abandoned.is_empty() {
            parts.push(format!(
                "- {} task(s) abandoned (too old)",
                self.abandoned.len()
            ));
        }
        if self.orphan_containers > 0 {
            parts.push(format!(
                "- {} orphaned container(s) cleaned up",
                self.orphan_containers
            ));
        }

        parts.join("\n")
    }

    /// Returns true if no tasks needed recovery (feature spec: persistence-recovery 7.4).
    pub fn is_clean(&self) -> bool {
        self.retried.is_empty()
            && self.resumed.is_empty()
            && self.reprompted.is_empty()
            && self.abandoned.is_empty()
            && self.orphan_containers == 0
    }
}

/// Classify a task's recovery action based on its state and age (feature spec: persistence-recovery 7.2).
pub fn determine_recovery_action(
    task: &PersistedTask,
    max_age: Duration,
    now: DateTime<Utc>,
) -> RecoveryAction {
    let age = now.signed_duration_since(task.updated_at);
    if age > max_age {
        return RecoveryAction::Abandon;
    }

    match &task.state {
        PersistedTaskState::Extracting | PersistedTaskState::Planning => {
            RecoveryAction::RetryFromScratch
        }

        PersistedTaskState::Executing {
            completed_steps, ..
        } => {
            if completed_steps.is_empty() {
                RecoveryAction::RetryFromScratch
            } else {
                RecoveryAction::ResumeExecution
            }
        }

        PersistedTaskState::Synthesizing => RecoveryAction::Resynthesize,

        PersistedTaskState::AwaitingApproval { .. } => RecoveryAction::RepromptApproval,

        PersistedTaskState::AwaitingCredential { .. } => RecoveryAction::RepromptCredential,

        // Terminal states should not appear in recovery (loaded via load_incomplete_tasks).
        PersistedTaskState::Completed
        | PersistedTaskState::Failed
        | PersistedTaskState::Abandoned => RecoveryAction::Abandon,
    }
}

/// Determine how to handle a specific step during execution resume (feature spec: persistence-recovery 7.3).
pub fn determine_step_recovery(step: &CompletedStep, was_in_progress: bool) -> StepRecovery {
    if step.action_semantics == "read" {
        // Reads are safe to retry, but if we have the result, skip.
        if step.result_json != serde_json::Value::Null {
            StepRecovery::SkipWithCachedResult
        } else {
            StepRecovery::Retry
        }
    } else {
        // Write semantics.
        if was_in_progress {
            StepRecovery::RequireOwnerConfirmation {
                message: format!(
                    "I was interrupted while executing '{}'. \
                     It may have already completed. Should I retry it?",
                    step.tool
                ),
            }
        } else {
            StepRecovery::Execute
        }
    }
}

/// Load incomplete tasks, classify them, mark stale ones abandoned, and produce a report
/// (feature spec: persistence-recovery 7.2).
///
/// Note: actual re-execution of recovered tasks is deferred. This function
/// classifies tasks and marks abandoned ones, returning the report for
/// owner notification.
pub fn recover_tasks(
    journal: &TaskJournal,
    max_age: Duration,
) -> Result<RecoveryReport, JournalError> {
    let now = Utc::now();
    let mut report = RecoveryReport::default();

    let incomplete = journal.load_incomplete_tasks()?;
    info!(
        count = incomplete.len(),
        "loaded incomplete tasks for recovery"
    );

    for task in &incomplete {
        let action = determine_recovery_action(task, max_age, now);

        match action {
            RecoveryAction::RetryFromScratch => {
                info!(task_id = %task.task_id, state = ?task.state, "recovery: retry from scratch");
                report.retried.push(task.task_id);
            }
            RecoveryAction::ResumeExecution => {
                info!(task_id = %task.task_id, state = ?task.state, "recovery: resume execution");
                report.resumed.push(task.task_id);
            }
            RecoveryAction::Resynthesize => {
                info!(task_id = %task.task_id, "recovery: resynthesize");
                report.resumed.push(task.task_id);
            }
            RecoveryAction::RepromptApproval => {
                info!(task_id = %task.task_id, "recovery: reprompt approval");
                report.reprompted.push(task.task_id);
            }
            RecoveryAction::RepromptCredential => {
                info!(task_id = %task.task_id, "recovery: reprompt credential");
                report.reprompted.push(task.task_id);
            }
            RecoveryAction::Abandon => {
                warn!(task_id = %task.task_id, state = ?task.state, "recovery: abandoning task");
                if let Err(e) = journal.mark_abandoned(
                    task.task_id,
                    "Abandoned after restart: exceeded max recovery age",
                ) {
                    warn!(task_id = %task.task_id, error = %e, "failed to mark task abandoned");
                }
                report.abandoned.push(task.task_id);
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::journal::{CreateTaskParams, TaskJournal};
    use crate::types::SecurityLabel;

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
            trace_id: None,
        }
    }

    // ── determine_recovery_action tests ──

    #[test]
    fn test_extracting_retries_from_scratch() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        let task = j.get_task(id).expect("get");
        let action = determine_recovery_action(&task, Duration::minutes(10), Utc::now());
        assert_eq!(action, RecoveryAction::RetryFromScratch);
    }

    #[test]
    fn test_planning_retries_from_scratch() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        j.update_state(id, &PersistedTaskState::Planning)
            .expect("update");
        let task = j.get_task(id).expect("get");
        let action = determine_recovery_action(&task, Duration::minutes(10), Utc::now());
        assert_eq!(action, RecoveryAction::RetryFromScratch);
    }

    #[test]
    fn test_executing_no_steps_retries_from_scratch() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        j.update_state(
            id,
            &PersistedTaskState::Executing {
                current_step: 0,
                completed_steps: vec![],
            },
        )
        .expect("update");
        let task = j.get_task(id).expect("get");
        let action = determine_recovery_action(&task, Duration::minutes(10), Utc::now());
        assert_eq!(action, RecoveryAction::RetryFromScratch);
    }

    #[test]
    fn test_executing_with_steps_resumes() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        j.update_state(
            id,
            &PersistedTaskState::Executing {
                current_step: 0,
                completed_steps: vec![],
            },
        )
        .expect("update state");
        let step = CompletedStep {
            step: 1,
            tool: "email.list".to_owned(),
            action_semantics: "read".to_owned(),
            result_json: serde_json::json!({"emails": []}),
            label: SecurityLabel::Sensitive,
            completed_at: Utc::now(),
        };
        j.append_completed_step(id, &step).expect("append");
        let task = j.get_task(id).expect("get");
        let action = determine_recovery_action(&task, Duration::minutes(10), Utc::now());
        assert_eq!(action, RecoveryAction::ResumeExecution);
    }

    #[test]
    fn test_synthesizing_resynthesizes() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        j.update_state(id, &PersistedTaskState::Synthesizing)
            .expect("update");
        let task = j.get_task(id).expect("get");
        let action = determine_recovery_action(&task, Duration::minutes(10), Utc::now());
        assert_eq!(action, RecoveryAction::Resynthesize);
    }

    #[test]
    fn test_awaiting_approval_reprompts() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        j.update_state(
            id,
            &PersistedTaskState::AwaitingApproval {
                approval_id: Uuid::new_v4(),
                step: 2,
            },
        )
        .expect("update");
        let task = j.get_task(id).expect("get");
        let action = determine_recovery_action(&task, Duration::minutes(10), Utc::now());
        assert_eq!(action, RecoveryAction::RepromptApproval);
    }

    #[test]
    fn test_awaiting_credential_reprompts() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        j.update_state(
            id,
            &PersistedTaskState::AwaitingCredential {
                service: "notion".to_owned(),
                prompt_message_id: None,
            },
        )
        .expect("update");
        let task = j.get_task(id).expect("get");
        let action = determine_recovery_action(&task, Duration::minutes(10), Utc::now());
        assert_eq!(action, RecoveryAction::RepromptCredential);
    }

    #[test]
    fn test_max_age_abandons() {
        let j = make_journal();
        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        j.update_state(id, &PersistedTaskState::Planning)
            .expect("update");
        let task = j.get_task(id).expect("get");
        // Simulate task being 15 minutes old with a 10 minute max age.
        let future = Utc::now() + Duration::minutes(15);
        let action = determine_recovery_action(&task, Duration::minutes(10), future);
        assert_eq!(action, RecoveryAction::Abandon);
    }

    // ── determine_step_recovery tests ──

    #[test]
    fn test_read_step_with_result_skips() {
        let step = CompletedStep {
            step: 1,
            tool: "email.list".to_owned(),
            action_semantics: "read".to_owned(),
            result_json: serde_json::json!({"emails": []}),
            label: SecurityLabel::Sensitive,
            completed_at: Utc::now(),
        };
        let recovery = determine_step_recovery(&step, false);
        assert_eq!(recovery, StepRecovery::SkipWithCachedResult);
    }

    #[test]
    fn test_read_step_no_result_retries() {
        let step = CompletedStep {
            step: 1,
            tool: "email.list".to_owned(),
            action_semantics: "read".to_owned(),
            result_json: serde_json::Value::Null,
            label: SecurityLabel::Sensitive,
            completed_at: Utc::now(),
        };
        let recovery = determine_step_recovery(&step, false);
        assert_eq!(recovery, StepRecovery::Retry);
    }

    #[test]
    fn test_write_step_not_in_progress_executes() {
        let step = CompletedStep {
            step: 2,
            tool: "email.send".to_owned(),
            action_semantics: "write".to_owned(),
            result_json: serde_json::Value::Null,
            label: SecurityLabel::Sensitive,
            completed_at: Utc::now(),
        };
        let recovery = determine_step_recovery(&step, false);
        assert_eq!(recovery, StepRecovery::Execute);
    }

    #[test]
    fn test_write_step_in_progress_requires_confirmation() {
        let step = CompletedStep {
            step: 2,
            tool: "email.send".to_owned(),
            action_semantics: "write".to_owned(),
            result_json: serde_json::Value::Null,
            label: SecurityLabel::Sensitive,
            completed_at: Utc::now(),
        };
        let recovery = determine_step_recovery(&step, true);
        assert!(matches!(
            recovery,
            StepRecovery::RequireOwnerConfirmation { .. }
        ));
        if let StepRecovery::RequireOwnerConfirmation { message } = recovery {
            assert!(message.contains("email.send"));
        }
    }

    // ── recover_tasks tests ──

    #[test]
    fn test_recover_tasks_mixed_states() {
        let j = make_journal();

        // Task 1: Planning (young) -> retry
        let id1 = Uuid::new_v4();
        j.create_task(&simple_params(id1)).expect("create");
        j.update_state(id1, &PersistedTaskState::Planning)
            .expect("update");

        // Task 2: AwaitingApproval (young) -> reprompt
        let id2 = Uuid::new_v4();
        j.create_task(&simple_params(id2)).expect("create");
        j.update_state(
            id2,
            &PersistedTaskState::AwaitingApproval {
                approval_id: Uuid::new_v4(),
                step: 1,
            },
        )
        .expect("update");

        // Task 3: Completed -> should not appear in recovery
        let id3 = Uuid::new_v4();
        j.create_task(&simple_params(id3)).expect("create");
        j.mark_completed(id3).expect("complete");

        let report = recover_tasks(&j, Duration::minutes(10)).expect("recover");
        assert_eq!(report.retried.len(), 1);
        assert!(report.retried.contains(&id1));
        assert_eq!(report.reprompted.len(), 1);
        assert!(report.reprompted.contains(&id2));
        assert!(report.abandoned.is_empty());
    }

    #[test]
    fn test_recover_tasks_abandons_old() {
        let j = make_journal();

        let id = Uuid::new_v4();
        j.create_task(&simple_params(id)).expect("create");
        j.update_state(id, &PersistedTaskState::Planning)
            .expect("update");

        // Use a very short max_age so the task is "old" immediately.
        let report = recover_tasks(&j, Duration::seconds(0)).expect("recover");
        assert_eq!(report.abandoned.len(), 1);
        assert!(report.abandoned.contains(&id));
        assert!(report.retried.is_empty());

        // Verify the task was actually marked abandoned in the journal.
        let task = j.get_task(id).expect("get");
        assert!(matches!(task.state, PersistedTaskState::Abandoned));
    }

    #[test]
    fn test_report_format_message_with_tasks() {
        let report = RecoveryReport {
            retried: vec![Uuid::new_v4()],
            resumed: vec![Uuid::new_v4(), Uuid::new_v4()],
            reprompted: vec![Uuid::new_v4()],
            abandoned: vec![Uuid::new_v4()],
            orphan_containers: 3,
        };
        let msg = report.format_message();
        assert!(msg.contains("Recovery report"));
        assert!(msg.contains("1 task(s) retried"));
        assert!(msg.contains("2 task(s) resumed"));
        assert!(msg.contains("1 approval/credential"));
        assert!(msg.contains("1 task(s) abandoned"));
        assert!(msg.contains("3 orphaned container"));
    }

    #[test]
    fn test_report_format_message_clean() {
        let report = RecoveryReport::default();
        assert!(report.is_clean());
        let msg = report.format_message();
        assert!(msg.contains("No pending tasks to recover"));
    }

    #[test]
    fn test_report_is_clean() {
        let mut report = RecoveryReport::default();
        assert!(report.is_clean());
        report.retried.push(Uuid::new_v4());
        assert!(!report.is_clean());
    }
}
