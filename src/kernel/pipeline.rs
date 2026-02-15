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
use tracing::{info, warn};

use crate::extractors::message::MessageIntentExtractor;
use crate::extractors::{ExtractedMetadata, Extractor};
use crate::kernel::audit::AuditLogger;
use crate::kernel::egress::EgressValidator;
use crate::kernel::executor::{ExecutorError, PlanExecutor};
use crate::kernel::inference::InferenceProxy;
use crate::kernel::journal::{SaveWorkingMemoryParams, TaskJournal};
use crate::kernel::planner::{strip_reasoning_tags, Planner, PlannerContext};
use crate::kernel::policy::{self, PolicyEngine};
use crate::kernel::session::{ConversationTurn, SessionStore, StructuredToolOutput, TaskResult};
use crate::kernel::synthesizer::{OutputInstructions, StepResult, Synthesizer, SynthesizerContext};
use crate::kernel::template::TaskTemplate;
use crate::tools::ToolRegistry;
use crate::types::{LabeledEvent, Principal, SecurityLabel, TaintLevel, Task, TaskState};

use chrono::Utc;

/// Maximum characters stored in request/response summaries for session memory.
const SUMMARY_MAX_CHARS: usize = 300;

/// Sentinel value stored in journal when persona onboarding is in progress
/// (persona-onboarding spec §3).
const PERSONA_PENDING: &str = "__pending__";

/// Maximum length for persona string (persona-onboarding spec §3).
const PERSONA_MAX_LEN: usize = 500;

/// Minimum length for a persona reply to be accepted.
/// Short replies like "Hi" or "Ok" are not valid persona configurations
/// and should re-trigger onboarding.
const PERSONA_MIN_LEN: usize = 10;

/// Truncate text to SUMMARY_MAX_CHARS for session storage (spec 9.1).
fn truncate_summary(text: &str) -> String {
    text.chars().take(SUMMARY_MAX_CHARS).collect()
}

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
    /// True when persona was stored this turn (pfar-system-identity-document.md §4).
    /// Signals the caller to rebuild the SID.
    pub persona_changed: bool,
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
    /// Optional task journal for crash recovery (feature spec: persistence-recovery).
    journal: Option<Arc<TaskJournal>>,
    /// System Identity Document -- shared, refreshable from main event loop
    /// (pfar-system-identity-document.md).
    sid: Arc<RwLock<String>>,
}

impl Pipeline {
    /// Create a new pipeline orchestrator (spec 7).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        policy: Arc<PolicyEngine>,
        inference: Arc<InferenceProxy>,
        executor: PlanExecutor,
        sessions: Arc<RwLock<SessionStore>>,
        egress: EgressValidator,
        tools: Arc<ToolRegistry>,
        audit: Arc<AuditLogger>,
        journal: Option<Arc<TaskJournal>>,
        sid: Arc<RwLock<String>>,
    ) -> Self {
        Self {
            policy,
            inference,
            executor,
            sessions,
            egress,
            tools,
            audit,
            journal,
            sid,
        }
    }

    /// Best-effort journal write — logs warning on failure, never fails the pipeline.
    fn journal_write<F>(&self, op_name: &str, f: F)
    where
        F: FnOnce(&TaskJournal) -> Result<(), crate::kernel::journal::JournalError>,
    {
        if let Some(ref j) = self.journal {
            if let Err(e) = f(j) {
                warn!(op = op_name, error = %e, "journal write failed (best-effort)");
            }
        }
    }

    /// Load persona string from journal (persona-onboarding spec §3).
    fn load_persona(&self) -> Option<String> {
        let journal = self.journal.as_ref()?;
        match journal.get_persona() {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "failed to load persona from journal");
                None
            }
        }
    }

    /// Store persona string to journal, best-effort (persona-onboarding spec §3).
    fn store_persona(&self, value: &str) {
        self.journal_write("set_persona", |j| j.set_persona(value));
    }

    /// Search long-term memory for entries relevant to extracted metadata (memory spec §6).
    ///
    /// Builds search terms from entities and dates in the metadata, queries journal
    /// FTS5 index, and returns formatted memory strings for prompt injection.
    /// Enforces label ceiling (Invariant C — No Read Up).
    fn search_memory(
        &self,
        metadata: &ExtractedMetadata,
        data_ceiling: SecurityLabel,
    ) -> Vec<String> {
        let journal = match self.journal.as_ref() {
            Some(j) => j,
            None => return vec![],
        };

        // Build search terms from entities and dates.
        let mut terms: Vec<&str> = Vec::new();
        for entity in &metadata.entities {
            terms.push(&entity.value);
        }
        for date in &metadata.dates_mentioned {
            terms.push(date);
        }

        if terms.is_empty() {
            return vec![];
        }

        // Join terms with OR for FTS5 query.
        let query = terms.join(" OR ");

        match journal.search_memories(&query, data_ceiling, 5) {
            Ok(rows) => rows
                .iter()
                .map(|r| {
                    let date = r.created_at.format("%b %d");
                    format!("{} ({})", r.content, date)
                })
                .collect(),
            Err(e) => {
                warn!(error = %e, "memory search failed (best-effort)");
                vec![]
            }
        }
    }

    /// Extract structured metadata from raw event text (spec 7, Phase 0).
    fn extract_metadata(
        &self,
        raw_text: &str,
        labeled_event: &mut LabeledEvent,
    ) -> ExtractedMetadata {
        let extractor = MessageIntentExtractor;
        let metadata = extractor.extract(raw_text);

        // Taint transition: structured fields get Extracted taint (spec 4.4).
        if labeled_event.taint.level == TaintLevel::Raw {
            labeled_event.taint.level = TaintLevel::Extracted;
            labeled_event
                .taint
                .touched_by
                .push(extractor.name().to_owned());
        }

        metadata
    }

    /// Check if this task needs the full pipeline with tool execution (spec 7, fast path).
    ///
    /// Returns `false` only for greetings and casual chat (short social messages).
    /// All other messages go through the Planner so the LLM can decide whether
    /// tools are needed — more reliable than keyword-based intent matching.
    fn should_use_full_pipeline(
        &self,
        metadata: &ExtractedMetadata,
        _template: &TaskTemplate,
    ) -> bool {
        !metadata.is_greeting
    }

    /// Execute Phases 1-2: Plan and Execute (spec 7, Phases 1-2).
    ///
    /// Returns tool execution results, final data label, and pipeline path indicator.
    async fn execute_full_pipeline(
        &self,
        task: &mut Task,
        template: &TaskTemplate,
        metadata: &ExtractedMetadata,
        labeled_event: &LabeledEvent,
        memory_entries: Vec<String>,
        sid: Option<String>,
    ) -> Result<
        (
            Vec<crate::kernel::executor::StepExecutionResult>,
            SecurityLabel,
            &'static str,
        ),
        PipelineError,
    > {
        // === PHASE 1: PLAN (spec 7, Phase 1) ===
        task.state = TaskState::Planning;
        info!(task_id = %task.task_id, pipeline_path = "full", "phase 1: planning");

        let planner_ctx = self
            .build_planner_context(task, template, metadata, memory_entries, sid)
            .await;
        let prompt = Planner::compose_prompt(&planner_ctx);

        let plan_response = self
            .inference
            .generate_with_config(
                &template.inference,
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

        Ok((exec_results, data_label, "full"))
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
        mut labeled_event: LabeledEvent,
        task: &mut Task,
        template: &TaskTemplate,
    ) -> Result<PipelineOutput, PipelineError> {
        // Read System Identity Document once per request
        // (pfar-system-identity-document.md).
        let sid_text = {
            let lock = self.sid.read().await;
            if lock.is_empty() {
                None
            } else {
                Some(lock.clone())
            }
        };

        // === PHASE 0: EXTRACT (spec 7, Phase 0) ===
        task.state = TaskState::Extracting;
        info!(task_id = %task.task_id, "phase 0: extracting metadata");

        let raw_text = labeled_event.event.payload.text.clone().unwrap_or_default();
        let metadata = self.extract_metadata(&raw_text, &mut labeled_event);

        // === MEMORY SEARCH (memory spec §6) ===
        // Search long-term memory for entries relevant to extracted metadata.
        // Uses label ceiling from task to enforce No Read Up (Invariant C).
        let memory_entries = self.search_memory(&metadata, task.data_ceiling);
        if !memory_entries.is_empty() {
            info!(task_id = %task.task_id, count = memory_entries.len(), "memory: found relevant entries");
        }

        // === PERSONA CHECK (persona-onboarding spec §3, §5) ===
        // Only runs when journal is available (persona requires persistence).
        let mut persona: Option<String> = None;
        let mut is_onboarding = false;
        let mut is_persona_just_configured = false;
        let mut force_fast_path = false;

        if self.journal.is_some() {
            persona = self.load_persona();
            info!(persona_state = ?persona.as_deref().map(|p| if p == PERSONA_PENDING { "__pending__" } else { "<set>" }), "persona loaded");

            if matches!(task.principal, Principal::Owner) {
                match persona.as_deref() {
                    None => {
                        // First message ever — mark pending, trigger onboarding.
                        info!("persona: first owner message, triggering onboarding");
                        self.store_persona(PERSONA_PENDING);
                        persona = None;
                        is_onboarding = true;
                        force_fast_path = true;
                    }
                    Some(PERSONA_PENDING) => {
                        // Second message — store owner's reply as real persona.
                        let trimmed = raw_text.trim();
                        if trimmed.is_empty() || trimmed.chars().count() < PERSONA_MIN_LEN {
                            // Too short to be a valid persona config — re-trigger onboarding.
                            info!(
                                reply_len = trimmed.len(),
                                "persona: reply too short, re-triggering onboarding"
                            );
                            is_onboarding = true;
                            persona = None;
                        } else {
                            let capped: String = trimmed.chars().take(PERSONA_MAX_LEN).collect();
                            info!("persona: storing owner's configuration");
                            self.store_persona(&capped);
                            persona = Some(capped);
                            is_persona_just_configured = true;
                        }
                        force_fast_path = true;
                    }
                    Some(_) => {
                        info!("persona: already configured, normal flow");
                    }
                }
            } else {
                // Non-owner: filter out pending sentinel.
                if persona.as_deref() == Some(PERSONA_PENDING) {
                    persona = None;
                }
            }
        }

        // === FAST PATH CHECK (spec 7, fast path) ===
        let needs_tools = !force_fast_path && self.should_use_full_pipeline(&metadata, template);

        let (exec_results, data_label, pipeline_path) = if needs_tools {
            self.execute_full_pipeline(
                task,
                template,
                &metadata,
                &labeled_event,
                memory_entries.clone(),
                sid_text.clone(),
            )
            .await?
        } else {
            // Fast path: no tools needed, skip directly to Phase 3 (spec 7, fast path).
            info!(task_id = %task.task_id, pipeline_path = "fast", "skipping planner — no tools needed");
            (vec![], labeled_event.label, "fast")
        };

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

        // Read session data for synthesizer context (spec 9.3).
        // Always provided — the Synthesizer prompt includes anti-summarization
        // instructions (spec 13.4, rule 4) to prevent history recap.
        let (working_memory, conversation) = self.read_session_data(&task.principal).await;

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
            session_working_memory: working_memory,
            conversation_history: conversation,
            persona,
            is_onboarding,
            is_persona_just_configured,
            memory_entries,
            sid: sid_text.clone(),
        };

        let synth_prompt = Synthesizer::compose_prompt(&synth_ctx);

        let raw_synth = self
            .inference
            .generate_with_config(
                &template.inference,
                &synth_prompt,
                template.max_tokens_synthesize,
                task.data_ceiling,
            )
            .await
            .map_err(|e| PipelineError::SynthesisFailed(e.to_string()))?;

        // Strip reasoning model tags (e.g. DeepSeek R1 <think>...</think>).
        let response_text = strip_reasoning_tags(&raw_synth).trim().to_owned();

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

        info!(task_id = %task.task_id, pipeline_path, "pipeline completed");

        Ok(PipelineOutput {
            response_text,
            output_sinks: task.output_sinks.clone(),
            data_label,
            persona_changed: is_persona_just_configured,
        })
    }

    /// Read session working memory and conversation history for a principal (spec 9.1, 9.2).
    ///
    /// Acquires a read lock on the session store, clones the data, then releases the lock.
    async fn read_session_data(
        &self,
        principal: &Principal,
    ) -> (Vec<TaskResult>, Vec<ConversationTurn>) {
        let sessions = self.sessions.read().await;
        let working_memory = sessions
            .get(principal)
            .map(|s| s.recent_results().iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let conversation = sessions
            .get(principal)
            .map(|s| s.conversation_history().iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        (working_memory, conversation)
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
        memory_entries: Vec<String>,
        sid: Option<String>,
    ) -> PlannerContext {
        let (working_memory, conversation) = self.read_session_data(&task.principal).await;

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
            memory_entries,
            sid,
        }
    }

    /// Store task results and conversation turns in session memory (spec 9.1, 9.2).
    ///
    /// Persists to both the in-memory session store and the SQLite journal
    /// so that conversation history survives process restarts.
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

        let request_summary = truncate_summary(raw_text);
        let response_summary = truncate_summary(response_text);
        let now = Utc::now();

        let task_result = TaskResult {
            task_id: task.task_id,
            timestamp: now,
            request_summary: request_summary.clone(),
            tool_outputs: structured_outputs.clone(),
            response_summary: response_summary.clone(),
            label: data_label,
        };

        let user_turn = ConversationTurn {
            role: "user".to_owned(),
            summary: truncate_summary(raw_text),
            timestamp: now,
        };
        let assistant_turn = ConversationTurn {
            role: "assistant".to_owned(),
            summary: truncate_summary(response_text),
            timestamp: now,
        };

        // Store in-memory.
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_or_create(&task.principal);
        session.push_result(task_result);
        session.push_turn(user_turn.clone());
        session.push_turn(assistant_turn.clone());

        // Persist to journal (best-effort, spec 9.1, 9.2).
        let principal_key = serde_json::to_string(&task.principal).unwrap_or_default();
        self.journal_write("save_user_turn", |j| {
            j.save_conversation_turn(&principal_key, &user_turn.role, &user_turn.summary, &now)
        });
        self.journal_write("save_assistant_turn", |j| {
            j.save_conversation_turn(
                &principal_key,
                &assistant_turn.role,
                &assistant_turn.summary,
                &now,
            )
        });
        let outputs_json = serde_json::to_string(&structured_outputs).unwrap_or_default();
        let task_id_for_memory = task.task_id;
        self.journal_write("save_working_memory", |j| {
            j.save_working_memory_result(&SaveWorkingMemoryParams {
                principal: &principal_key,
                task_id: task_id_for_memory,
                timestamp: &now,
                request_summary: &request_summary,
                tool_outputs_json: &outputs_json,
                response_summary: &response_summary,
                label: data_label,
            })
        });
        // Trim to capacity limits.
        self.journal_write("trim_turns", |j| {
            j.trim_conversation_turns(&principal_key, 20)
        });
        self.journal_write("trim_memory", |j| j.trim_working_memory(&principal_key, 10));
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

    /// Build a test pipeline with the given mock responses and optional tool registry.
    ///
    /// If `tools_fn` is None, registers MockEmailTool by default.
    fn make_pipeline_with_tools<F>(
        plan_json: &str,
        synth_text: &str,
        tools_fn: Option<F>,
    ) -> (Pipeline, Arc<RwLock<SessionStore>>)
    where
        F: FnOnce(&ToolRegistry),
    {
        let buf = SharedBuf::new();
        let policy = Arc::new(PolicyEngine::with_defaults());
        let audit = Arc::new(AuditLogger::from_writer(Box::new(buf)));

        let inference = Arc::new(InferenceProxy::with_provider(Box::new(
            MockPlannerProvider::new(plan_json, synth_text),
        )));

        let registry = ToolRegistry::new();
        if let Some(f) = tools_fn {
            f(&registry);
        } else {
            registry.register(Arc::new(MockEmailTool));
        }
        let tools = Arc::new(registry);

        let vault: Arc<dyn crate::kernel::vault::SecretStore> = Arc::new(InMemoryVault::new());
        let executor =
            PlanExecutor::new(policy.clone(), tools.clone(), vault.clone(), audit.clone());

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
            None, // No journal for basic pipeline tests.
            Arc::new(RwLock::new(String::new())),
        );

        (pipeline, sessions)
    }

    /// Build a test pipeline with default MockEmailTool registered.
    fn make_pipeline(plan_json: &str, synth_text: &str) -> (Pipeline, Arc<RwLock<SessionStore>>) {
        make_pipeline_with_tools(plan_json, synth_text, None::<fn(&ToolRegistry)>)
    }

    /// Build a test pipeline with an in-memory journal for persona tests.
    fn make_pipeline_with_journal(
        plan_json: &str,
        synth_text: &str,
    ) -> (Pipeline, Arc<RwLock<SessionStore>>, Arc<TaskJournal>) {
        let buf = SharedBuf::new();
        let policy = Arc::new(PolicyEngine::with_defaults());
        let audit = Arc::new(AuditLogger::from_writer(Box::new(buf)));

        let inference = Arc::new(InferenceProxy::with_provider(Box::new(
            MockPlannerProvider::new(plan_json, synth_text),
        )));

        let registry = ToolRegistry::new();
        registry.register(Arc::new(MockEmailTool));
        let tools = Arc::new(registry);

        let vault: Arc<dyn crate::kernel::vault::SecretStore> = Arc::new(InMemoryVault::new());
        let executor =
            PlanExecutor::new(policy.clone(), tools.clone(), vault.clone(), audit.clone());

        let sessions = Arc::new(RwLock::new(SessionStore::new()));
        let egress = EgressValidator::new(policy.clone(), audit.clone());

        let journal = Arc::new(TaskJournal::open_in_memory().expect("test journal"));

        let pipeline = Pipeline::new(
            policy,
            inference,
            executor,
            sessions.clone(),
            egress,
            tools,
            audit,
            Some(journal.clone()),
            Arc::new(RwLock::new(String::new())),
        );

        (pipeline, sessions, journal)
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
    /// Uses "check my email" (intent: email_check) to trigger the full path,
    /// but planner returns an empty plan.
    #[tokio::test]
    async fn test_pipeline_empty_plan() {
        let plan_json = r#"{"plan":[],"explanation":"No actionable emails found"}"#;
        let synth_text = "Your inbox looks clear right now.";
        let (pipeline, _sessions) = make_pipeline(plan_json, synth_text);

        let event = make_labeled_event("check my email", Principal::Owner);
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
    /// Uses "check my email" to trigger the full path with an empty plan.
    #[tokio::test]
    async fn test_pipeline_stores_conversation_turns() {
        let plan_json = r#"{"plan":[]}"#;
        let synth_text = "No new emails.";
        let (pipeline, sessions) = make_pipeline(plan_json, synth_text);

        let event = make_labeled_event("check my email", Principal::Owner);
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
        assert!(history[0].summary.contains("check my email"));
        assert_eq!(history[1].role, "assistant");
        assert!(history[1].summary.contains("No new emails"));
    }

    /// Egress denied: data label exceeds sink label, pipeline returns EgressDenied.
    ///
    /// Regression test 7: regulated data cannot egress to WhatsApp (public sink).
    #[tokio::test]
    async fn test_pipeline_egress_denied() {
        let plan_json = r#"{"plan":[]}"#;
        let synth_text = "Here is your health report.";
        let (pipeline, _sessions) = make_pipeline(plan_json, synth_text);

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
        let plan_json = r#"{"plan":[],"explanation":"Cannot schedule without calendar access"}"#;
        let synth_text = "I don't have access to scheduling tools right now.";
        // No tools registered for this test.
        let (pipeline, sessions) =
            make_pipeline_with_tools(plan_json, synth_text, Some(|_reg: &ToolRegistry| {}));

        let template = make_third_party_template();
        let principal = Principal::WhatsAppContact("+34665030077".to_owned());
        let event = make_labeled_event("schedule a meeting for next Tuesday", principal.clone());
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

    // ── Fast path tests ──

    /// Fast path: "Hey!" has no intent → skips planner, goes directly to synthesize.
    /// The mock provider's first call returns plan_response (which is our synth text
    /// on the fast path since planner is skipped). If the planner WERE called, it
    /// would try to parse this as JSON and fail.
    #[tokio::test]
    async fn test_fast_path_greeting() {
        let synth_text = "Hello! How can I help you today?";
        // plan_response = synth_text because on fast path the first inference call
        // is synthesis, not planning. synth_response is never reached.
        let (pipeline, _sessions) = make_pipeline(synth_text, "ERROR: second call unexpected");

        let event = make_labeled_event("Hey!", Principal::Owner);
        let template = make_template(); // has email tools
        let mut task = make_task(&template);

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(result.is_ok(), "fast path should succeed: {result:?}");

        let output = result.expect("checked");
        assert_eq!(output.response_text, synth_text);
        assert!(matches!(task.state, TaskState::Completed));
    }

    /// Full path: "check my email" has email_check intent matching email.* tools.
    #[tokio::test]
    async fn test_full_path_email_check() {
        let plan_json =
            r#"{"plan":[{"step":1,"tool":"email.list","args":{"account":"personal","limit":10}}]}"#;
        let synth_text = "You have 2 new emails.";
        let (pipeline, _sessions) = make_pipeline(plan_json, synth_text);

        let event = make_labeled_event("check my email", Principal::Owner);
        let template = make_template();
        let mut task = make_task(&template);

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(result.is_ok(), "full path should succeed: {result:?}");

        let output = result.expect("checked");
        assert_eq!(output.response_text, synth_text);
    }

    /// Empty template tools: Planner is called but returns empty plan,
    /// pipeline proceeds to synthesize without tool execution.
    #[tokio::test]
    async fn test_empty_template_tools_planner_returns_empty_plan() {
        let empty_plan = r#"{"plan":[],"explanation":"no tools available"}"#;
        let synth_text = "I can help with that!";
        let (pipeline, _sessions) = make_pipeline(empty_plan, synth_text);

        let event = make_labeled_event("check my email", Principal::Owner);
        let mut template = make_template();
        template.allowed_tools = vec![]; // No tools available.
        let mut task = make_task(&template);

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(result.is_ok(), "empty-plan path should succeed: {result:?}");

        let output = result.expect("checked");
        assert_eq!(output.response_text, synth_text);
    }

    /// Fast path still stores working memory and conversation turns.
    #[tokio::test]
    async fn test_fast_path_stores_session() {
        let synth_text = "Hey there! What can I do for you?";
        let (pipeline, sessions) = make_pipeline(synth_text, "ERROR: second call unexpected");

        let event = make_labeled_event("hello", Principal::Owner);
        let template = make_template();
        let mut task = make_task(&template);

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(result.is_ok());

        // Verify session has working memory.
        let store = sessions.read().await;
        let session = store
            .get(&Principal::Owner)
            .expect("owner session should exist after fast path");

        assert_eq!(
            session.recent_results().len(),
            1,
            "fast path should store task result in working memory"
        );
        let task_result = &session.recent_results()[0];
        assert!(
            task_result.tool_outputs.is_empty(),
            "fast path should have empty tool outputs"
        );

        // Verify conversation history.
        let history = session.conversation_history();
        assert_eq!(history.len(), 2, "should have user + assistant turns");
        assert_eq!(history[0].role, "user");
        assert!(history[0].summary.contains("hello"));
        assert_eq!(history[1].role, "assistant");
        assert!(history[1].summary.contains("Hey there"));
    }

    /// Fast path omits session context from synthesizer prompt (spec 9.3, 13.5).
    ///
    /// After a full-path turn populates session data, a subsequent fast-path
    /// turn must NOT include conversation history or working memory in the
    /// synthesizer prompt — otherwise the LLM summarizes them.
    #[tokio::test]
    async fn test_fast_path_includes_session_context() {
        // A prompt-capturing provider: records each prompt it receives.
        struct CapturingProvider {
            responses: Vec<String>,
            call_count: AtomicUsize,
            captured_prompts: Arc<Mutex<Vec<String>>>,
        }

        #[async_trait]
        impl InferenceProvider for CapturingProvider {
            async fn generate(
                &self,
                _model: &str,
                prompt: &str,
                _max_tokens: u32,
            ) -> Result<String, InferenceError> {
                self.captured_prompts
                    .lock()
                    .expect("test lock")
                    .push(prompt.to_owned());
                let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
                Ok(self
                    .responses
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| "fallback".to_owned()))
            }
        }

        let captured = Arc::new(Mutex::new(Vec::new()));

        // Responses: [0] = plan for turn 1, [1] = synth for turn 1,
        //            [2] = synth for turn 2 (fast path, skips planner)
        let provider = CapturingProvider {
            responses: vec![
                // Turn 1: plan response (full path)
                r#"{"plan":[{"step":1,"tool":"email.list","args":{"limit":5}}]}"#.to_owned(),
                // Turn 1: synth response
                "You have 2 emails from Sarah and GitHub.".to_owned(),
                // Turn 2: synth response (fast path — this is the first inference call)
                "Hey there!".to_owned(),
            ],
            call_count: AtomicUsize::new(0),
            captured_prompts: captured.clone(),
        };

        let buf = SharedBuf::new();
        let policy = Arc::new(PolicyEngine::with_defaults());
        let audit = Arc::new(AuditLogger::from_writer(Box::new(buf)));
        let inference = Arc::new(InferenceProxy::with_provider(Box::new(provider)));
        let registry = ToolRegistry::new();
        registry.register(Arc::new(MockEmailTool));
        let tools = Arc::new(registry);
        let vault: Arc<dyn crate::kernel::vault::SecretStore> = Arc::new(InMemoryVault::new());
        let executor =
            PlanExecutor::new(policy.clone(), tools.clone(), vault.clone(), audit.clone());
        let sessions = Arc::new(RwLock::new(SessionStore::new()));
        let egress = EgressValidator::new(policy.clone(), audit.clone());
        let pipeline = Pipeline::new(
            policy,
            inference,
            executor,
            sessions,
            egress,
            tools,
            audit,
            None,
            Arc::new(RwLock::new(String::new())),
        );

        let template = make_template();

        // Turn 1: full path — "check my email" populates session data.
        let event1 = make_labeled_event("check my email", Principal::Owner);
        let mut task1 = make_task(&template);
        let result1 = pipeline.run(event1, &mut task1, &template).await;
        assert!(result1.is_ok(), "turn 1 should succeed: {result1:?}");

        // Turn 2: fast path — "hi" MUST still include session context (spec 9.3).
        let event2 = make_labeled_event("hi", Principal::Owner);
        let mut task2 = make_task(&template);
        let result2 = pipeline.run(event2, &mut task2, &template).await;
        assert!(result2.is_ok(), "turn 2 should succeed: {result2:?}");

        // The third captured prompt is the fast-path synthesizer call.
        let prompts = captured.lock().expect("test lock");
        assert!(
            prompts.len() >= 3,
            "expected at least 3 inference calls, got {}",
            prompts.len()
        );
        let fast_path_synth_prompt = &prompts[2];

        // Fast-path prompt MUST contain session context so the LLM has
        // continuity across turns (spec 9.3). Anti-summarization is handled
        // by the Synthesizer prompt (spec 13.4, rule 4), not by data withholding.
        assert!(
            fast_path_synth_prompt.contains("## Conversation History"),
            "fast-path synth prompt MUST include conversation history"
        );
        // Sanity: it should still contain the user's current message.
        assert!(
            fast_path_synth_prompt.contains("hi"),
            "fast-path synth prompt should include current message"
        );
    }

    /// Fast path includes SID in Synthesizer prompt (pfar-system-identity-document.md).
    #[tokio::test]
    async fn test_fast_path_includes_sid() {
        struct CapturingProvider {
            captured_prompts: Arc<Mutex<Vec<String>>>,
        }

        #[async_trait]
        impl InferenceProvider for CapturingProvider {
            async fn generate(
                &self,
                _model: &str,
                prompt: &str,
                _max_tokens: u32,
            ) -> Result<String, InferenceError> {
                self.captured_prompts
                    .lock()
                    .expect("test lock")
                    .push(prompt.to_owned());
                Ok("Hello!".to_owned())
            }
        }

        let captured = Arc::new(Mutex::new(Vec::new()));
        let provider = CapturingProvider {
            captured_prompts: captured.clone(),
        };

        let buf = SharedBuf::new();
        let policy = Arc::new(PolicyEngine::with_defaults());
        let audit = Arc::new(AuditLogger::from_writer(Box::new(buf)));
        let inference = Arc::new(InferenceProxy::with_provider(Box::new(provider)));
        let registry = ToolRegistry::new();
        registry.register(Arc::new(MockEmailTool));
        let tools = Arc::new(registry);
        let vault: Arc<dyn crate::kernel::vault::SecretStore> = Arc::new(InMemoryVault::new());
        let executor =
            PlanExecutor::new(policy.clone(), tools.clone(), vault.clone(), audit.clone());
        let sessions = Arc::new(RwLock::new(SessionStore::new()));
        let egress = EgressValidator::new(policy.clone(), audit.clone());
        let journal = Arc::new(TaskJournal::open_in_memory().expect("test journal"));

        // Pre-set persona so onboarding doesn't trigger.
        journal.set_persona("Atlas").expect("set persona");

        // Pre-populate the SID with real content.
        let sid = Arc::new(RwLock::new(
            "You are Atlas.\n\nCAPABILITIES:\n- Built-in tools: email\n\nRULES:\n- Never mention internal architecture\n".to_owned(),
        ));

        let pipeline = Pipeline::new(
            policy,
            inference,
            executor,
            sessions,
            egress,
            tools,
            audit,
            Some(journal),
            sid,
        );

        // "hello" triggers fast path (no tools needed).
        let event = make_labeled_event("hello", Principal::Owner);
        let template = make_template();
        let mut task = make_task(&template);

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(result.is_ok(), "fast path should succeed: {result:?}");

        // Fast path = 1 inference call (Synthesizer only, no Planner).
        let prompts = captured.lock().expect("test lock");
        assert_eq!(
            prompts.len(),
            1,
            "fast path should have exactly 1 inference call"
        );

        let synth_prompt = &prompts[0];
        assert!(
            synth_prompt.contains("CAPABILITIES:"),
            "fast-path synth prompt should contain SID capabilities"
        );
        assert!(
            synth_prompt.contains("Built-in tools: email"),
            "fast-path synth prompt should contain SID tool list"
        );
    }

    // ── Existing planner context tests ──

    /// Verify that the pipeline correctly builds PlannerContext by exercising
    /// build_planner_context directly. For third-party triggers, the context
    /// must use planner_task_description.
    #[tokio::test]
    async fn test_build_planner_context_third_party() {
        let (pipeline, _sessions) = make_pipeline(r#"{"plan":[]}"#, "ok");

        let template = make_third_party_template();
        let principal = Principal::WhatsAppContact("+1234".to_owned());
        let mut task = make_task(&template);
        // Override the principal for third-party test.
        task.principal = principal;

        let metadata = crate::extractors::ExtractedMetadata {
            intent: Some("scheduling".to_owned()),
            entities: vec![],
            dates_mentioned: vec!["next Tuesday".to_owned()],
            extra: serde_json::Value::Null,
            is_greeting: false,
        };

        let ctx = pipeline
            .build_planner_context(&task, &template, &metadata, vec![], None)
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

    // ── Persona lifecycle tests (persona-onboarding spec §3, §5) ──

    /// First owner message with no persona → onboarding prompt, fast path.
    #[tokio::test]
    async fn test_pipeline_first_message_onboarding() {
        let synth_text = "Hello! I'm your new assistant. What should I call myself?";
        let (pipeline, _sessions, journal) =
            make_pipeline_with_journal(synth_text, "ERROR: second call unexpected");

        // No persona set — journal is fresh.
        let event = make_labeled_event("Hey!", Principal::Owner);
        let template = make_template();
        let mut task = make_task(&template);

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(result.is_ok(), "onboarding should succeed: {result:?}");

        // Journal should have __pending__ sentinel.
        let stored = journal.get_persona().expect("get").expect("should exist");
        assert_eq!(stored, PERSONA_PENDING);

        // Response should be the onboarding synth text.
        let output = result.expect("checked");
        assert_eq!(output.response_text, synth_text);
    }

    /// Second owner message with __pending__ → stores real persona.
    #[tokio::test]
    async fn test_pipeline_second_message_stores_persona() {
        let synth_text = "Got it, Igor.";
        let (pipeline, _sessions, journal) =
            make_pipeline_with_journal(synth_text, "ERROR: second call unexpected");

        // Pre-set the pending sentinel (simulates first message already happened).
        journal.set_persona(PERSONA_PENDING).expect("set pending");

        let event = make_labeled_event(
            "Call yourself Atlas. I'm Igor. Keep it concise.",
            Principal::Owner,
        );
        let template = make_template();
        let mut task = make_task(&template);

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(result.is_ok(), "persona store should succeed: {result:?}");

        // Journal should now have the real persona.
        let stored = journal.get_persona().expect("get").expect("should exist");
        assert_eq!(stored, "Call yourself Atlas. I'm Igor. Keep it concise.");
    }

    /// Owner message with existing persona → persona injected into synth prompt.
    #[tokio::test]
    async fn test_pipeline_persona_in_normal_flow() {
        // Use a prompt-capturing provider to verify persona appears in the synth prompt.
        struct CapturingProvider {
            response: String,
            captured_prompts: Arc<Mutex<Vec<String>>>,
        }

        #[async_trait]
        impl InferenceProvider for CapturingProvider {
            async fn generate(
                &self,
                _model: &str,
                prompt: &str,
                _max_tokens: u32,
            ) -> Result<String, InferenceError> {
                self.captured_prompts
                    .lock()
                    .expect("test lock")
                    .push(prompt.to_owned());
                Ok(self.response.clone())
            }
        }

        let captured = Arc::new(Mutex::new(Vec::new()));
        let provider = CapturingProvider {
            response: "Hey Igor, what's up?".to_owned(),
            captured_prompts: captured.clone(),
        };

        let buf = SharedBuf::new();
        let policy = Arc::new(PolicyEngine::with_defaults());
        let audit = Arc::new(AuditLogger::from_writer(Box::new(buf)));
        let inference = Arc::new(InferenceProxy::with_provider(Box::new(provider)));
        let registry = ToolRegistry::new();
        registry.register(Arc::new(MockEmailTool));
        let tools = Arc::new(registry);
        let vault: Arc<dyn crate::kernel::vault::SecretStore> = Arc::new(InMemoryVault::new());
        let executor =
            PlanExecutor::new(policy.clone(), tools.clone(), vault.clone(), audit.clone());
        let sessions = Arc::new(RwLock::new(SessionStore::new()));
        let egress = EgressValidator::new(policy.clone(), audit.clone());
        let journal = Arc::new(TaskJournal::open_in_memory().expect("test journal"));

        // Pre-set a real persona.
        journal
            .set_persona("Atlas. Owner: Igor. Style: concise.")
            .expect("set persona");

        let pipeline = Pipeline::new(
            policy,
            inference,
            executor,
            sessions,
            egress,
            tools,
            audit,
            Some(journal),
            Arc::new(RwLock::new(String::new())),
        );

        let event = make_labeled_event("Hey!", Principal::Owner);
        let template = make_template();
        let mut task = make_task(&template);

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(result.is_ok(), "normal flow should succeed: {result:?}");

        // The synthesizer prompt (first inference call, since fast path) should contain the persona.
        let prompts = captured.lock().expect("test lock");
        assert!(
            !prompts.is_empty(),
            "should have at least one inference call"
        );
        assert!(
            prompts[0].contains("You are Atlas. Owner: Igor. Style: concise."),
            "synth prompt should contain persona"
        );
        assert!(
            prompts[0].contains("Never mention internal system details"),
            "synth prompt should contain anti-leak instruction"
        );
    }

    /// Short reply like "Hi" with __pending__ → re-triggers onboarding, does NOT store.
    #[tokio::test]
    async fn test_pipeline_short_reply_retriggers_onboarding() {
        let synth_text = "I still need your configuration! What should I call myself?";
        let (pipeline, _sessions, journal) =
            make_pipeline_with_journal(synth_text, "ERROR: second call unexpected");

        // Pre-set the pending sentinel.
        journal.set_persona(PERSONA_PENDING).expect("set pending");

        let event = make_labeled_event("Hi", Principal::Owner);
        let template = make_template();
        let mut task = make_task(&template);

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(result.is_ok(), "short reply should succeed: {result:?}");

        // Journal should still have __pending__ — "Hi" is too short.
        let stored = journal.get_persona().expect("get").expect("should exist");
        assert_eq!(
            stored, PERSONA_PENDING,
            "short reply should NOT overwrite pending"
        );
    }

    /// Non-owner message with no persona → default prompt, no onboarding.
    #[tokio::test]
    async fn test_pipeline_non_owner_no_onboarding() {
        let synth_text = "I can help with scheduling.";
        let (pipeline, _sessions, journal) =
            make_pipeline_with_journal(synth_text, "ERROR: second call unexpected");

        // No persona in journal.
        let principal = Principal::TelegramPeer("12345".to_owned());
        let event = make_labeled_event("Hey!", principal.clone());
        let template = make_template();
        let mut task = make_task(&template);
        task.principal = principal;
        // Route to owner sink so egress passes for Internal-labeled data.
        task.output_sinks = vec!["sink:telegram:owner".to_owned()];

        let result = pipeline.run(event, &mut task, &template).await;
        assert!(result.is_ok(), "non-owner should succeed: {result:?}");

        // Journal should NOT have a persona set (non-owner doesn't trigger onboarding).
        let stored = journal.get_persona().expect("get");
        assert!(
            stored.is_none(),
            "non-owner should not trigger persona onboarding"
        );
    }

    // ── Memory search tests (memory spec §6) ──

    #[test]
    fn test_search_memory_empty_when_no_entities() {
        let (pipeline, _sessions, _journal) = make_pipeline_with_journal(r#"{"plan":[]}"#, "ok");

        let metadata = ExtractedMetadata {
            intent: Some("email_check".to_owned()),
            entities: vec![],
            dates_mentioned: vec![],
            extra: serde_json::Value::Null,
            is_greeting: false,
        };

        let results = pipeline.search_memory(&metadata, SecurityLabel::Sensitive);
        assert!(results.is_empty(), "no entities/dates should return empty");
    }

    #[test]
    fn test_search_memory_finds_saved_entries() {
        use crate::kernel::journal::MemoryRow;

        let (pipeline, _sessions, journal) = make_pipeline_with_journal(r#"{"plan":[]}"#, "ok");

        // Save a memory about a flight to Bali.
        let row = MemoryRow {
            id: uuid::Uuid::new_v4().to_string(),
            content: "Flight to Bali is on March 15th".to_owned(),
            label: SecurityLabel::Sensitive,
            source: "explicit".to_owned(),
            created_at: chrono::Utc::now(),
            task_id: None,
        };
        journal.save_memory(&row).expect("save");

        // Search with entity matching "Bali".
        let metadata = ExtractedMetadata {
            intent: Some("scheduling".to_owned()),
            entities: vec![crate::extractors::ExtractedEntity {
                kind: "location".to_owned(),
                value: "Bali".to_owned(),
            }],
            dates_mentioned: vec![],
            extra: serde_json::Value::Null,
            is_greeting: false,
        };

        let results = pipeline.search_memory(&metadata, SecurityLabel::Sensitive);
        assert_eq!(results.len(), 1, "should find one memory entry");
        assert!(
            results[0].contains("Flight to Bali"),
            "result should contain the memory content"
        );
    }

    #[test]
    fn test_search_memory_label_filtering() {
        use crate::kernel::journal::MemoryRow;

        let (pipeline, _sessions, journal) = make_pipeline_with_journal(r#"{"plan":[]}"#, "ok");

        // Save a sensitive memory.
        let row = MemoryRow {
            id: uuid::Uuid::new_v4().to_string(),
            content: "Doctor appointment Tuesday".to_owned(),
            label: SecurityLabel::Sensitive,
            source: "explicit".to_owned(),
            created_at: chrono::Utc::now(),
            task_id: None,
        };
        journal.save_memory(&row).expect("save");

        // Search with Internal ceiling — should NOT find Sensitive memory.
        let metadata = ExtractedMetadata {
            intent: None,
            entities: vec![crate::extractors::ExtractedEntity {
                kind: "person".to_owned(),
                value: "Doctor".to_owned(),
            }],
            dates_mentioned: vec![],
            extra: serde_json::Value::Null,
            is_greeting: false,
        };

        let results = pipeline.search_memory(&metadata, SecurityLabel::Internal);
        assert!(
            results.is_empty(),
            "sensitive memory should not be visible at internal ceiling"
        );
    }

    #[test]
    fn test_search_memory_no_journal() {
        let (pipeline, _sessions) = make_pipeline(r#"{"plan":[]}"#, "ok");

        let metadata = ExtractedMetadata {
            intent: None,
            entities: vec![crate::extractors::ExtractedEntity {
                kind: "person".to_owned(),
                value: "Sarah".to_owned(),
            }],
            dates_mentioned: vec![],
            extra: serde_json::Value::Null,
            is_greeting: false,
        };

        // Pipeline without journal should return empty gracefully.
        let results = pipeline.search_memory(&metadata, SecurityLabel::Sensitive);
        assert!(results.is_empty(), "no journal should return empty");
    }
}
