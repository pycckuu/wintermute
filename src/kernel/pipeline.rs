//! Plan-Then-Execute pipeline -- the core execution model (spec 7).
//!
//! Orchestrates four phases:
//! - Phase 0 (Extract): Structured metadata extraction
//! - Phase 1 (Plan): LLM-generated execution plan
//! - Phase 2 (Execute): Mechanical tool dispatch
//! - Phase 3 (Synthesize): LLM-generated response
//!
//! Then validates egress and stores results in session working memory.

use std::sync::Arc;

use thiserror::Error;
use tokio::sync::RwLock;
use tracing::info;

use crate::extractors::message::MessageIntentExtractor;
use crate::extractors::{ExtractedMetadata, Extractor};
use crate::kernel::audit::AuditLogger;
use crate::kernel::egress::EgressValidator;
use crate::kernel::executor::{ExecutorError, PlanExecutor};
use crate::kernel::inference::InferenceProxy;
use crate::kernel::planner::{Planner, PlannerContext};
use crate::kernel::policy::{self, PolicyEngine};
use crate::kernel::session::{ConversationTurn, SessionStore, StructuredToolOutput, TaskResult};
use crate::kernel::synthesizer::{OutputInstructions, StepResult, Synthesizer, SynthesizerContext};
use crate::kernel::template::TaskTemplate;
use crate::tools::ToolRegistry;
use crate::types::{LabeledEvent, SecurityLabel, TaintLevel, Task, TaskState};

use chrono::Utc;

/// Maximum characters stored in request/response summaries for session memory.
const SUMMARY_MAX_CHARS: usize = 100;

/// Pipeline errors (spec 7, 14.6).
#[derive(Debug, Error)]
pub enum PipelineError {
    /// Phase 0 extraction failed.
    #[error("extraction failed: {0}")]
    ExtractionFailed(String),
    /// Phase 1 planning failed.
    #[error("planning failed: {0}")]
    PlanningFailed(String),
    /// Phase 2 execution failed.
    #[error("execution failed: {0}")]
    ExecutionFailed(String),
    /// Phase 3 synthesis failed.
    #[error("synthesis failed: {0}")]
    SynthesisFailed(String),
    /// Egress validation denied delivery to the target sink.
    #[error("egress denied: {0}")]
    EgressDenied(String),
    /// A write operation requires human approval before proceeding.
    #[error("approval required: {0}")]
    ApprovalRequired(String),
}

/// Output from a completed pipeline run (spec 7).
#[derive(Debug, Clone)]
pub struct PipelineOutput {
    /// Response text to send to the user.
    pub response_text: String,
    /// Sinks to deliver the response to.
    pub output_sinks: Vec<String>,
    /// Highest security label of data involved.
    pub data_label: SecurityLabel,
}

/// The Plan-Then-Execute pipeline (spec 7).
///
/// Wires together extraction, planning, execution, synthesis, and
/// egress validation into a single orchestration flow. Each phase
/// enforces the spec's security invariants.
pub struct Pipeline {
    policy: Arc<PolicyEngine>,
    inference: Arc<InferenceProxy>,
    executor: PlanExecutor,
    sessions: Arc<RwLock<SessionStore>>,
    egress: EgressValidator,
    tools: Arc<ToolRegistry>,
    #[allow(dead_code)] // Used by future pipeline phases for direct audit logging.
    audit: Arc<AuditLogger>,
}

impl Pipeline {
    /// Create a new pipeline orchestrator (spec 7).
    pub fn new(
        policy: Arc<PolicyEngine>,
        inference: Arc<InferenceProxy>,
        executor: PlanExecutor,
        sessions: Arc<RwLock<SessionStore>>,
        egress: EgressValidator,
        tools: Arc<ToolRegistry>,
        audit: Arc<AuditLogger>,
    ) -> Self {
        Self {
            policy,
            inference,
            executor,
            sessions,
            egress,
            tools,
            audit,
        }
    }

    /// Run the full 4-phase pipeline (spec 7).
    ///
    /// Phases:
    /// 0. Extract structured metadata from the raw event
    /// 1. Plan an execution via LLM (no raw content, no tools)
    /// 2. Execute the plan mechanically (no LLM)
    /// 3. Synthesize a response via LLM (sees content, no tools)
    ///
    /// Then validates egress and stores results in session working memory.
    pub async fn run(
        &self,
        labeled_event: LabeledEvent,
        task: &mut Task,
        template: &TaskTemplate,
    ) -> Result<PipelineOutput, PipelineError> {
        // === PHASE 0: EXTRACT (spec 7, Phase 0) ===
        task.state = TaskState::Extracting;
        info!(task_id = %task.task_id, "phase 0: extracting metadata");

        let extractor = MessageIntentExtractor;
        let raw_text = labeled_event.event.payload.text.clone().unwrap_or_default();
        let metadata = extractor.extract(&raw_text);

        // Taint transition: structured fields get Extracted taint (spec 4.4).
        let mut extracted_taint = labeled_event.taint.clone();
        if extracted_taint.level == TaintLevel::Raw {
            extracted_taint.level = TaintLevel::Extracted;
            extracted_taint.touched_by.push(extractor.name().to_owned());
        }

        // === PHASE 1: PLAN (spec 7, Phase 1) ===
        task.state = TaskState::Planning;
        info!(task_id = %task.task_id, "phase 1: planning");

        let planner_ctx = self.build_planner_context(task, template, &metadata).await;

        let prompt = Planner::compose_prompt(&planner_ctx);

        let plan_response = self
            .inference
            .generate(
                &template.inference.model,
                &prompt,
                template.max_tokens_plan,
                task.data_ceiling,
            )
            .await
            .map_err(|e| PipelineError::PlanningFailed(e.to_string()))?;

        let plan = Planner::parse_plan(&plan_response)
            .map_err(|e| PipelineError::PlanningFailed(e.to_string()))?;

        Planner::validate_plan(&plan, &task.allowed_tools, &task.denied_tools)
            .map_err(|e| PipelineError::PlanningFailed(e.to_string()))?;

        // === PHASE 2: EXECUTE (spec 7, Phase 2) ===
        task.state = TaskState::Executing { current_step: 0 };
        info!(task_id = %task.task_id, steps = plan.plan.len(), "phase 2: executing plan");

        // Convert planner PlanSteps to executor PlanSteps.
        let exec_steps: Vec<crate::kernel::executor::PlanStep> = plan
            .plan
            .iter()
            .map(|s| crate::kernel::executor::PlanStep {
                step: s.step,
                tool: s.tool.clone(),
                args: s.args.clone(),
            })
            .collect();

        // Use original event taint for write approval checks (not extracted).
        let exec_results = self
            .executor
            .execute_plan(task, &exec_steps, &labeled_event.taint)
            .await
            .map_err(|e| match e {
                ExecutorError::ApprovalRequired(reason) => PipelineError::ApprovalRequired(reason),
                other => PipelineError::ExecutionFailed(other.to_string()),
            })?;

        // Propagate labels: result inherits max of all labels (spec 4.3).
        let mut all_labels: Vec<SecurityLabel> = exec_results.iter().map(|r| r.label).collect();
        all_labels.push(labeled_event.label);
        let data_label = self.policy.propagate_label(&all_labels);

        // === PHASE 3: SYNTHESIZE (spec 7, Phase 3) ===
        task.state = TaskState::Synthesizing;
        info!(task_id = %task.task_id, "phase 3: synthesizing response");

        let step_results: Vec<StepResult> = exec_results
            .iter()
            .map(|r| StepResult {
                step: r.step,
                tool: r.tool.clone(),
                result: r.output.clone(),
            })
            .collect();

        let default_sink = "sink:default".to_owned();
        let first_sink = task.output_sinks.first().unwrap_or(&default_sink);

        let synth_ctx = SynthesizerContext {
            task_id: task.task_id,
            original_context: raw_text.clone(),
            raw_content_ref: None, // Vault raw content refs are a future feature.
            tool_results: step_results,
            output_instructions: OutputInstructions {
                sink: first_sink.clone(),
                max_length: 2000,
                format: "plain_text".to_owned(),
            },
        };

        let synth_prompt = Synthesizer::compose_prompt(&synth_ctx);

        let response_text = self
            .inference
            .generate(
                &template.inference.model,
                &synth_prompt,
                template.max_tokens_synthesize,
                task.data_ceiling,
            )
            .await
            .map_err(|e| PipelineError::SynthesisFailed(e.to_string()))?;

        // === EGRESS VALIDATION (spec 10.8) ===
        for sink in &task.output_sinks {
            self.egress
                .validate_and_log(data_label, sink, response_text.len())
                .map_err(|e| PipelineError::EgressDenied(e.to_string()))?;
        }

        // === STORE IN SESSION (spec 9.1, 9.2) ===
        self.store_session_results(task, &raw_text, &exec_results, &response_text, data_label)
            .await;

        // Mark task complete.
        task.state = TaskState::Completed;
        info!(task_id = %task.task_id, "pipeline completed");

        Ok(PipelineOutput {
            response_text,
            output_sinks: task.output_sinks.clone(),
            data_label,
        })
    }

    /// Build the PlannerContext from task, template, and extracted metadata (spec 10.3).
    ///
    /// For third-party triggers, uses `planner_task_description` instead of
    /// the template description (Invariant E, regression test 13).
    async fn build_planner_context(
        &self,
        task: &Task,
        template: &TaskTemplate,
        metadata: &ExtractedMetadata,
    ) -> PlannerContext {
        // Read session data under the lock, then release.
        let (working_memory, conversation) = {
            let sessions = self.sessions.read().await;
            let memory = sessions
                .get(&task.principal)
                .map(|s| s.recent_results().iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            let history = sessions
                .get(&task.principal)
                .map(|s| s.conversation_history().iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            (memory, history)
        };

        // Get available tools for this template.
        let available_tools = self
            .tools
            .available_actions(&task.allowed_tools, &task.denied_tools);

        let principal_class = policy::resolve_principal_class(&task.principal);

        PlannerContext {
            task_id: task.task_id,
            template_description: template.description.clone(),
            planner_task_description: template.planner_task_description.clone(),
            extracted_metadata: metadata.clone(),
            session_working_memory: working_memory,
            conversation_history: conversation,
            available_tools,
            principal_class,
        }
    }

    /// Store task results and conversation turns in session memory (spec 9.1, 9.2).
    async fn store_session_results(
        &self,
        task: &Task,
        raw_text: &str,
        exec_results: &[crate::kernel::executor::StepExecutionResult],
        response_text: &str,
        data_label: SecurityLabel,
    ) {
        let structured_outputs: Vec<StructuredToolOutput> = exec_results
            .iter()
            .map(|r| StructuredToolOutput {
                tool: r.tool.clone(),
                action: r.tool.clone(),
                output: r.output.clone(),
                label: r.label,
            })
            .collect();

        let request_summary: String = raw_text.chars().take(SUMMARY_MAX_CHARS).collect();
        let response_summary: String = response_text.chars().take(SUMMARY_MAX_CHARS).collect();

        let task_result = TaskResult {
            task_id: task.task_id,
            timestamp: Utc::now(),
            request_summary,
            tool_outputs: structured_outputs,
            response_summary,
            label: data_label,
        };

        let user_turn = ConversationTurn {
            role: "user".to_owned(),
            summary: raw_text.chars().take(SUMMARY_MAX_CHARS).collect(),
            timestamp: Utc::now(),
        };
        let assistant_turn = ConversationTurn {
            role: "assistant".to_owned(),
            summary: response_text.chars().take(SUMMARY_MAX_CHARS).collect(),
            timestamp: Utc::now(),
        };

        let mut sessions = self.sessions.write().await;
        let session = sessions.get_or_create(&task.principal);
        session.push_result(task_result);
        session.push_turn(user_turn);
        session.push_turn(assistant_turn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::audit::AuditLogger;
    use crate::kernel::egress::EgressValidator;
    use crate::kernel::executor::PlanExecutor;
    use crate::kernel::inference::{InferenceError, InferenceProvider, InferenceProxy};
    use crate::kernel::policy::PolicyEngine;
    use crate::kernel::session::SessionStore;
    use crate::kernel::template::{InferenceConfig, TaskTemplate};
    use crate::kernel::vault::InMemoryVault;
    use crate::tools::scoped_http::ScopedHttpClient;
    use crate::tools::{
        ActionSemantics, InjectedCredentials, Tool, ToolAction, ToolError, ToolManifest,
        ToolOutput, ToolRegistry, ValidatedCapability,
    };
    use crate::types::{
        EventKind, EventPayload, EventSource, InboundEvent, LabeledEvent, Principal,
        PrincipalClass, SecurityLabel, TaintLevel, TaintSet, Task, TaskState,
    };

    use async_trait::async_trait;
    use std::io::{Cursor, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    // ── Mock inference provider ──

    /// Returns predetermined responses: first call returns the plan JSON,
    /// subsequent calls return the synthesis response.
    struct MockPlannerProvider {
        plan_response: String,
        synth_response: String,
        call_count: AtomicUsize,
    }

    impl MockPlannerProvider {
        fn new(plan_response: &str, synth_response: &str) -> Self {
            Self {
                plan_response: plan_response.to_owned(),
                synth_response: synth_response.to_owned(),
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl InferenceProvider for MockPlannerProvider {
        async fn generate(
            &self,
            _model: &str,
            _prompt: &str,
            _max_tokens: u32,
        ) -> Result<String, InferenceError> {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                Ok(self.plan_response.clone())
            } else {
                Ok(self.synth_response.clone())
            }
        }
    }

    // ── Mock tool ──

    struct MockEmailTool;

    #[async_trait]
    impl Tool for MockEmailTool {
        fn manifest(&self) -> ToolManifest {
            ToolManifest {
                name: "email".to_owned(),
                owner_only: false,
                actions: vec![
                    ToolAction {
                        id: "email.list".to_owned(),
                        description: "List recent emails".to_owned(),
                        semantics: ActionSemantics::Read,
                        label_ceiling: SecurityLabel::Sensitive,
                        args_schema: serde_json::json!({"account": "string", "limit": "integer"}),
                    },
                    ToolAction {
                        id: "email.read".to_owned(),
                        description: "Read a specific email".to_owned(),
                        semantics: ActionSemantics::Read,
                        label_ceiling: SecurityLabel::Sensitive,
                        args_schema: serde_json::json!({"message_id": "string"}),
                    },
                ],
                network_allowlist: vec!["mail.zoho.eu".to_owned()],
            }
        }

        async fn execute(
            &self,
            _cap: &ValidatedCapability,
            _creds: &InjectedCredentials,
            _http: &ScopedHttpClient,
            action: &str,
            _args: serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            match action {
                "email.list" => Ok(ToolOutput {
                    data: serde_json::json!({
                        "emails": [
                            {"id": "msg_1", "from": "sarah@co", "subject": "Q3 Budget"},
                            {"id": "msg_2", "from": "github", "subject": "[PR #42] Fix auth"}
                        ]
                    }),
                    has_free_text: false,
                }),
                "email.read" => Ok(ToolOutput {
                    data: serde_json::json!({
                        "id": "msg_1",
                        "from": "sarah@co",
                        "subject": "Q3 Budget",
                        "body": "Please review the Q3 budget."
                    }),
                    has_free_text: true,
                }),
                other => Err(ToolError::ActionNotFound(other.to_owned())),
            }
        }
    }

    // ── Shared audit buffer ──

    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Cursor<Vec<u8>>>>);

    impl SharedBuf {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Cursor::new(Vec::new()))))
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

    // ── Helper builders ──

    fn make_template() -> TaskTemplate {
        TaskTemplate {
            template_id: "test_owner_general".to_owned(),
            triggers: vec!["adapter:telegram:message:owner".to_owned()],
            principal_class: PrincipalClass::Owner,
            description: "General assistant for owner via Telegram".to_owned(),
            planner_task_description: None,
            allowed_tools: vec!["email.list".to_owned(), "email.read".to_owned()],
            denied_tools: vec![],
            max_tool_calls: 10,
            max_tokens_plan: 4000,
            max_tokens_synthesize: 8000,
            output_sinks: vec!["sink:telegram:owner".to_owned()],
            data_ceiling: SecurityLabel::Sensitive,
            inference: InferenceConfig {
                provider: "local".to_owned(),
                model: "llama3".to_owned(),
                owner_acknowledged_cloud_risk: false,
            },
            require_approval_for_writes: false,
        }
    }

    fn make_third_party_template() -> TaskTemplate {
        TaskTemplate {
            template_id: "whatsapp_scheduling".to_owned(),
            triggers: vec!["adapter:whatsapp:message:third_party".to_owned()],
            principal_class: PrincipalClass::ThirdParty,
            description: "Handle scheduling requests from contacts".to_owned(),
            planner_task_description: Some(
                "A contact is requesting to schedule a meeting.".to_owned(),
            ),
            allowed_tools: vec!["calendar.freebusy".to_owned(), "message.reply".to_owned()],
            denied_tools: vec![],
            max_tool_calls: 5,
            max_tokens_plan: 2000,
            max_tokens_synthesize: 2000,
            output_sinks: vec!["sink:whatsapp:reply_to_sender".to_owned()],
            data_ceiling: SecurityLabel::Internal,
            inference: InferenceConfig {
                provider: "local".to_owned(),
                model: "llama3".to_owned(),
                owner_acknowledged_cloud_risk: false,
            },
            require_approval_for_writes: false,
        }
    }

    fn make_task(template: &TaskTemplate) -> Task {
        Task {
            task_id: Uuid::nil(),
            template_id: template.template_id.clone(),
            principal: Principal::Owner,
            trigger_event: Uuid::nil(),
            data_ceiling: template.data_ceiling,
            allowed_tools: template.allowed_tools.clone(),
            denied_tools: template.denied_tools.clone(),
            max_tool_calls: template.max_tool_calls,
            output_sinks: template.output_sinks.clone(),
            trace_id: "test-trace".to_owned(),
            state: TaskState::Extracting,
        }
    }

    fn make_labeled_event(text: &str, principal: Principal) -> LabeledEvent {
        let taint_level = match &principal {
            Principal::Owner => TaintLevel::Clean,
            _ => TaintLevel::Raw,
        };
        let origin = match &principal {
            Principal::Owner => "owner".to_owned(),
            Principal::WhatsAppContact(phone) => format!("adapter:whatsapp:{phone}"),
            _ => "external".to_owned(),
        };
        LabeledEvent {
            event: InboundEvent {
                event_id: Uuid::new_v4(),
                timestamp: Utc::now(),
                source: EventSource {
                    adapter: "telegram".to_owned(),
                    principal: principal.clone(),
                },
                kind: EventKind::Message,
                payload: EventPayload {
                    text: Some(text.to_owned()),
                    attachments: vec![],
                    reply_to: None,
                    metadata: serde_json::Value::Null,
                },
            },
            label: match &principal {
                Principal::Owner => SecurityLabel::Sensitive,
                _ => SecurityLabel::Internal,
            },
            taint: TaintSet {
                level: taint_level,
                origin,
                touched_by: vec![],
            },
        }
    }

    fn make_pipeline(plan_json: &str, synth_text: &str) -> (Pipeline, Arc<RwLock<SessionStore>>) {
        let buf = SharedBuf::new();
        let policy = Arc::new(PolicyEngine::with_defaults());
        let audit = Arc::new(AuditLogger::from_writer(Box::new(buf)));

        let inference = Arc::new(InferenceProxy::with_provider(Box::new(
            MockPlannerProvider::new(plan_json, synth_text),
        )));

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockEmailTool));
        let tools = Arc::new(registry);

        let vault: Arc<dyn crate::kernel::vault::SecretStore> = Arc::new(InMemoryVault::new());
        let executor = PlanExecutor::new(policy.clone(), tools.clone(), vault, audit.clone());

        let sessions = Arc::new(RwLock::new(SessionStore::new()));
        let egress = EgressValidator::new(policy.clone(), audit.clone());

        let pipeline = Pipeline::new(
            policy,
            inference,
            executor,
            sessions.clone(),
            egress,
            tools,
            audit,
        );

        (pipeline, sessions)
    }

    // ── Tests ──

    /// Full pipeline: owner asks "check my email", planner returns a plan
    /// with email.list, tools execute, synthesizer responds.
    #[tokio::test]
    async fn test_full_pipeline_email_check() {
        let plan_json =
            r#"{"plan":[{"step":1,"tool":"email.list","args":{"account":"personal","limit":10}}]}"#;
        let synth_text = "You have 2 new emails: one from Sarah about Q3 Budget and one from GitHub about PR #42.";
        let (pipeline, _sessions) = make_pipeline(plan_json, synth_text);

        let event = make_labeled_event("check my email", Principal::Owner);
        let template = make_template();
        let mut task = make_task(&template);

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(result.is_ok(), "pipeline should succeed: {result:?}");

        let output = result.expect("checked");
        assert_eq!(output.response_text, synth_text);
        assert_eq!(output.output_sinks, vec!["sink:telegram:owner"]);
        // Data label should be at least Sensitive (from event + email tool ceiling).
        assert!(output.data_label >= SecurityLabel::Sensitive);
        // Task should be marked Completed.
        assert!(
            matches!(task.state, TaskState::Completed),
            "task state should be Completed, got: {:?}",
            task.state
        );
    }

    /// Planner returns an empty plan; synthesizer still runs and delivers a response.
    #[tokio::test]
    async fn test_pipeline_empty_plan() {
        let plan_json = r#"{"plan":[],"explanation":"No tools needed for a greeting"}"#;
        let synth_text = "Hello! How can I help you today?";
        let (pipeline, _sessions) = make_pipeline(plan_json, synth_text);

        let event = make_labeled_event("hello", Principal::Owner);
        let template = make_template();
        let mut task = make_task(&template);

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(result.is_ok(), "empty plan should still succeed");

        let output = result.expect("checked");
        assert_eq!(output.response_text, synth_text);
        assert!(matches!(task.state, TaskState::Completed));
    }

    /// Regression test 17: after pipeline run, session has the task result
    /// in working memory so the next turn's Planner can see it.
    #[tokio::test]
    async fn test_pipeline_stores_working_memory() {
        let plan_json = r#"{"plan":[{"step":1,"tool":"email.list","args":{"limit":5}}]}"#;
        let synth_text = "Listed your emails.";
        let (pipeline, sessions) = make_pipeline(plan_json, synth_text);

        let event = make_labeled_event("check my email", Principal::Owner);
        let template = make_template();
        let mut task = make_task(&template);

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(result.is_ok());

        // Verify session working memory has the task result.
        let store = sessions.read().await;
        let session = store
            .get(&Principal::Owner)
            .expect("owner session should exist after pipeline run");

        assert_eq!(
            session.recent_results().len(),
            1,
            "should have 1 task result in working memory"
        );

        let task_result = &session.recent_results()[0];
        assert_eq!(task_result.task_id, Uuid::nil());
        assert!(!task_result.tool_outputs.is_empty());
        assert_eq!(task_result.tool_outputs[0].tool, "email.list");
    }

    /// After pipeline run, verify both user and assistant conversation turns are stored.
    #[tokio::test]
    async fn test_pipeline_stores_conversation_turns() {
        let plan_json = r#"{"plan":[]}"#;
        let synth_text = "Hi there!";
        let (pipeline, sessions) = make_pipeline(plan_json, synth_text);

        let event = make_labeled_event("hello", Principal::Owner);
        let template = make_template();
        let mut task = make_task(&template);

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(result.is_ok());

        let store = sessions.read().await;
        let session = store
            .get(&Principal::Owner)
            .expect("owner session should exist");

        let history = session.conversation_history();
        assert_eq!(history.len(), 2, "should have user + assistant turns");
        assert_eq!(history[0].role, "user");
        assert!(history[0].summary.contains("hello"));
        assert_eq!(history[1].role, "assistant");
        assert!(history[1].summary.contains("Hi there"));
    }

    /// Egress denied: data label exceeds sink label, pipeline returns EgressDenied.
    ///
    /// Regression test 7: regulated data cannot egress to WhatsApp (public sink).
    #[tokio::test]
    async fn test_pipeline_egress_denied() {
        // Use a plan that produces no tool results, but set the event label
        // to Regulated. The egress check against a public sink should fail.
        let plan_json = r#"{"plan":[]}"#;
        let synth_text = "Here is your health report.";
        let buf = SharedBuf::new();
        let policy = Arc::new(PolicyEngine::with_defaults());
        let audit = Arc::new(AuditLogger::from_writer(Box::new(buf)));

        let inference = Arc::new(InferenceProxy::with_provider(Box::new(
            MockPlannerProvider::new(plan_json, synth_text),
        )));

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockEmailTool));
        let tools = Arc::new(registry);
        let vault: Arc<dyn crate::kernel::vault::SecretStore> = Arc::new(InMemoryVault::new());
        let executor = PlanExecutor::new(policy.clone(), tools.clone(), vault, audit.clone());
        let sessions = Arc::new(RwLock::new(SessionStore::new()));
        let egress = EgressValidator::new(policy.clone(), audit.clone());

        let pipeline = Pipeline::new(policy, inference, executor, sessions, egress, tools, audit);

        // Event with Regulated label targeting a public sink.
        let mut event = make_labeled_event("health report", Principal::Owner);
        event.label = SecurityLabel::Regulated;

        let template = make_template();
        let mut task = make_task(&template);
        // Route to WhatsApp (public sink) instead of telegram:owner.
        task.output_sinks = vec!["sink:whatsapp:reply_to_sender".to_owned()];

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(result.is_err(), "should fail egress validation");

        let err = result.expect_err("checked");
        assert!(
            matches!(err, PipelineError::EgressDenied(_)),
            "expected EgressDenied, got: {err}"
        );
    }

    /// Third-party event: verify planner context uses planner_task_description
    /// (Invariant E, regression test 13).
    ///
    /// We cannot directly inspect the planner prompt in the full pipeline, but we
    /// can verify the planner context is built correctly by checking that the
    /// pipeline completes without error when using a third-party template. The
    /// planner_task_description prevents raw message injection.
    #[tokio::test]
    async fn test_pipeline_third_party_context() {
        // This plan uses no tools (empty plan) since we don't have calendar tools
        // registered. The key test is that the pipeline doesn't crash and the
        // planner context is correctly built for third-party triggers.
        let plan_json = r#"{"plan":[],"explanation":"Cannot schedule without calendar access"}"#;
        let synth_text = "I don't have access to scheduling tools right now.";
        let buf = SharedBuf::new();
        let policy = Arc::new(PolicyEngine::with_defaults());
        let audit = Arc::new(AuditLogger::from_writer(Box::new(buf)));

        let inference = Arc::new(InferenceProxy::with_provider(Box::new(
            MockPlannerProvider::new(plan_json, synth_text),
        )));

        let registry = ToolRegistry::new(); // No tools registered.
        let tools = Arc::new(registry);
        let vault: Arc<dyn crate::kernel::vault::SecretStore> = Arc::new(InMemoryVault::new());
        let executor = PlanExecutor::new(policy.clone(), tools.clone(), vault, audit.clone());
        let sessions = Arc::new(RwLock::new(SessionStore::new()));
        let egress = EgressValidator::new(policy.clone(), audit.clone());

        let pipeline = Pipeline::new(
            policy,
            inference,
            executor,
            sessions.clone(),
            egress,
            tools,
            audit,
        );

        let template = make_third_party_template();
        let principal = Principal::WhatsAppContact("+34665030077".to_owned());
        let event = make_labeled_event("Can we meet next Tuesday?", principal.clone());
        let mut task = Task {
            task_id: Uuid::nil(),
            template_id: template.template_id.clone(),
            principal: principal.clone(),
            trigger_event: Uuid::nil(),
            data_ceiling: template.data_ceiling,
            allowed_tools: template.allowed_tools.clone(),
            denied_tools: template.denied_tools.clone(),
            max_tool_calls: template.max_tool_calls,
            // Route to owner sink (Regulated level) so Internal data passes
            // egress. The real whatsapp sink (Public) would fail No Write Down
            // for Internal-labeled data -- that's correct policy behavior tested
            // separately in test_pipeline_egress_denied.
            output_sinks: vec!["sink:telegram:owner".to_owned()],
            trace_id: "test-trace-3rd".to_owned(),
            state: TaskState::Extracting,
        };

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(
            result.is_ok(),
            "third-party pipeline should succeed: {result:?}"
        );

        let output = result.expect("checked");
        assert_eq!(output.response_text, synth_text);

        // Verify the session is stored under the third-party principal.
        let store = sessions.read().await;
        let session = store.get(&principal);
        assert!(session.is_some(), "third-party session should be created");
    }

    /// Verify that the pipeline correctly builds PlannerContext by exercising
    /// build_planner_context directly. For third-party triggers, the context
    /// must use planner_task_description.
    #[tokio::test]
    async fn test_build_planner_context_third_party() {
        let (pipeline, _sessions) = make_pipeline(r#"{"plan":[]}"#, "ok");

        let template = make_third_party_template();
        let task = Task {
            task_id: Uuid::nil(),
            template_id: template.template_id.clone(),
            principal: Principal::WhatsAppContact("+1234".to_owned()),
            trigger_event: Uuid::nil(),
            data_ceiling: template.data_ceiling,
            allowed_tools: template.allowed_tools.clone(),
            denied_tools: template.denied_tools.clone(),
            max_tool_calls: template.max_tool_calls,
            output_sinks: template.output_sinks.clone(),
            trace_id: "test".to_owned(),
            state: TaskState::Extracting,
        };

        let metadata = crate::extractors::ExtractedMetadata {
            intent: Some("scheduling".to_owned()),
            entities: vec![],
            dates_mentioned: vec!["next Tuesday".to_owned()],
            extra: serde_json::Value::Null,
        };

        let ctx = pipeline
            .build_planner_context(&task, &template, &metadata)
            .await;

        // The planner_task_description should be set.
        assert_eq!(
            ctx.planner_task_description.as_deref(),
            Some("A contact is requesting to schedule a meeting.")
        );
        // The principal class should be ThirdParty.
        assert_eq!(ctx.principal_class, PrincipalClass::ThirdParty);
        // The prompt should use planner_task_description, not template_description.
        let prompt = Planner::compose_prompt(&ctx);
        assert!(
            prompt.contains("A contact is requesting to schedule a meeting."),
            "third-party prompt must use planner_task_description"
        );
        assert!(
            !prompt.contains("Handle scheduling requests from contacts"),
            "third-party prompt must NOT contain template_description"
        );
    }
}
