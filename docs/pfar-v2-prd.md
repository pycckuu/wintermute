# PFAR v2 — Implementation Tasks

> Based on spec: `docs/pfar-v2-spec.md` Section 19

## Progress

- [x] Phase 0: Repository setup from template
- [x] Phase 1: Kernel Core (73 tests passing — 66 unit + 7 integration)
- [x] Phase 2: Telegram + Pipeline + First Tools (274 tests — 248 unit + 7 integration + 19 regression)
- [x] Feature: Persistence & Recovery (362 tests — 324 unit + 7 integration + 31 regression)
- [x] Feature: Trim persistence to simplified spec (304 tests — 278 unit + 7 integration + 19 regression)
- [x] Feature: Pipeline Fast Path (316 tests — 290 unit + 7 integration + 19 regression)
- [x] Feature: TOML Configuration (329 tests — 303 unit + 7 integration + 19 regression)
- [x] Feature: Synthesizer Prompt Quality (331 tests — 305 unit + 7 integration + 19 regression)
- [x] Feature: Admin Tool + Credential Flow (349 tests — 320 unit + 7 integration + 22 regression)
- [x] Fix: Inference Routing Diagnostics + Rename `default_model` → `model` (349 tests)
- [x] Feature: Agent Persona & Onboarding (359 tests — 330 unit + 7 integration + 22 regression)
- [x] Feature: Memory System (378 tests — 349 unit + 7 integration + 22 regression)
- [x] Feature: Dynamic Integrations via MCP (399 tests — 370 unit + 7 integration + 22 regression)
- [x] Fix: Admin tool descriptions for MCP integration discovery (401 tests — 372 unit + 7 integration + 22 regression)
- [x] Fix: Admin tool label ceilings — Secret→Sensitive for non-secret actions (401 tests)
- [x] Feature: Credential Acquisition — In-Chat Paste with Kernel Intercept (423 tests — 394 unit + 7 integration + 22 regression)
- [x] Fix: Deterministic admin plan for setup flow — bypasses LLM planner for credential acquisition (433 tests — 404 unit + 7 integration + 22 regression)
- [x] Fix: Admin plan auto-connects when credential already exists in vault (434 tests — 405 unit + 7 integration + 22 regression)
- [x] Feature: KernelFlowManager — Integration Setup State Machine (435 tests — 406 unit + 7 integration + 22 regression)
- [x] Fix: Known MCP server package names — Notion, Fetch (435 tests)
- [x] Feature: System Identity Document (466 tests — 437 unit + 7 integration + 22 regression)
- [x] Feature: Find Local Skills — scan skills directory at startup, parse skill.toml, spawn as MCP (483 tests — 454 unit + 7 integration + 22 regression)

## Phase 1: Kernel Core (weeks 1-3)

Goal: Working kernel that receives events, matches templates, enforces policies.

- [x] 1.1 Core types: Principal, SecurityLabel, TaintSet, CapabilityToken, Task
- [x] 1.2 Event router (accepts test events via CLI adapter)
- [x] 1.3 Principal resolution
- [x] 1.4 Policy Engine: label assignment, propagation, No Read Up, No Write Down
- [x] 1.5 Policy Engine: taint checking (graduated rules)
- [x] 1.6 Policy Engine: capability token generation and validation
- [x] 1.7 Task template engine (load TOML, match triggers, validate plans)
- [x] 1.8 Vault abstraction (OS keychain for master key, SQLCipher for three DBs)
- [x] 1.9 Inference proxy (HTTP to Ollama on localhost)
- [x] 1.10 Audit logger (structured JSON)
- [x] 1.11 Unit tests for all Policy Engine functions
- [x] 1.12 Integration test: CLI event -> template match -> mock plan -> mock execute -> response

## Phase 2: Telegram + Pipeline + First Tools (weeks 4-5)

Goal: End-to-end flow: Telegram message -> extract -> plan -> execute -> synthesize -> reply.

- [x] 2.1 Telegram adapter (in-process async task, polling)
- [x] 2.2 Phase 0: Message intent extractor (simple classifier)
- [x] 2.3 Phase 1: Planner (LLM via inference proxy)
- [x] 2.4 Phase 2: Kernel plan executor (in-process tool dispatch)
- [x] 2.5 Phase 3: Synthesizer (LLM via inference proxy)
- [x] 2.6 Egress validation and message delivery
- [x] 2.7 Session working memory (per-principal, in vault)
- [x] 2.8 Conversation history (sliding window)
- [x] 2.9 Two read-only tools: calendar.freebusy, email.list + email.read
- [x] 2.10 ScopedHttpClient with domain allowlist + private IP blocking
- [x] 2.10b Pipeline orchestrator (4-phase Plan-Then-Execute, spec 7)
- [x] 2.11a Approval queue core (submit/resolve/timeout, 14 tests)
- [x] 2.11b-F1 Policy Engine: inference routing check (spec 11.1, 7 tests)
- [x] 2.11b-F2 InferenceProxy: generate_with_config with label-based routing (7 tests)
- [x] 2.11b-F3 main.rs: full startup wiring and Telegram event loop (spec 14.1)
- [x] 2.11b-F4 kernel/mod.rs: cleanup module declarations
- [ ] 2.11b Approval queue Telegram inline button integration
- [x] 2.12 Regression tests: 1, 2, 4, 5, 7, 8, 9, 13, 16, 17 (19 tests)

## Feature: Persistence (Simplified)

Goal: Session persistence, adapter state, graceful shutdown. No task journaling or recovery.
Spec: `docs/pfar-feature-persistence-recovery.md`

- [x] P.1 TaskJournal module — adapter_state + conversation_turns + working_memory tables
- [x] P.2 Pipeline session persistence — conversation history + working memory writes
- [x] P.3 Telegram adapter state persistence (4 tests)
- [x] P.4 Graceful shutdown — audit events + signal handling (2 tests)
- [x] P.5 Startup integration and owner notification — simple restart message
- [x] P.6 Session persistence — conversation history + working memory survive restarts (15 tests)
- [x] P.7 Trim: removed task journaling, recovery module, ~70 task lifecycle tests

## Feature: Pipeline Fast Path

Goal: Skip Planner for messages that don't need tools. Spec: `docs/pfar-feature-fast-path.md`

- [x] FP.1 Add `could_use(tool)` method to `ExtractedMetadata` (spec 6.10, 8 tests)
- [x] FP.2 Add fast path branch in `Pipeline::run()` after Phase 0 (spec 7, 4 tests)
- [x] FP.3 Pipeline path logging (`pipeline_path=fast|full`)
- [x] FP.4 Quality assurance (refactor, review, lint)

## Feature: TOML Configuration

Goal: Centralize config into ./config.toml with env var overrides. Spec: `docs/pfar-feature-config.md`

- [x] C.1 Config structs with serde Deserialize (spec 18.1, 14 tests)
- [x] C.2 Config loading from ./config.toml with defaults
- [x] C.3 Env var override layer (backward compat)
- [x] C.4 Refactor main.rs to use PfarConfig — remove hardcoded constants
- [x] C.5 Quality assurance (refactor, review, lint)
- [x] C.6 config.example.toml with documented defaults

## Feature: Synthesizer Prompt Quality

Goal: Prevent Synthesizer from summarizing conversation history on every response.

- [x] SQ.1 Update Synthesizer role prompt to prevent conversation summary repetition
- [x] SQ.2 Reformat conversation history from JSON blob to readable lines with anti-summary header
- [x] SQ.3 Add test verifying history format discourages summarization
- [x] SQ.4 Omit conversation history and working memory from Synthesizer on fast path

## Feature: Agent Persona & Onboarding

Goal: First-message onboarding that configures agent identity, persisted in journal.
Spec: `docs/pfar-feature-persona-onboarding.md`

- [x] PO.1 Persona table in journal.db + get/set methods (3 tests)
- [x] PO.2 Synthesizer persona-aware prompt composition (3 tests)
- [x] PO.3 Pipeline persona lifecycle (load/store/onboarding) (4 tests)
- [ ] PO.4 admin.update_persona action (deferred to later Phase 3 work)

## Feature: Memory System

Goal: Persistent, searchable, label-filtered long-term memory.
Spec: `docs/pfar-feature-memory.md`

- [x] M.1 memories table + FTS5 in journal.db with save/search methods (5 tests)
- [x] M.2 MemoryTool (memory.save action) + registration (4 tests)
- [x] M.3 memory_save intent detection + could_use mapping (4 tests)
- [x] M.4 Planner + Synthesizer memory_entries context fields + prompt injection
- [x] M.5 Pipeline memory search after Phase 0 + context injection (4 tests)
- [x] M.6 main.rs wiring: tool registration, template allowed_tools
- [x] M.7 Quality assurance (fmt, clippy, full test suite)

Deferred (needs scheduler):
- [ ] M.8 Daily consolidation cron job with label-aware LLM routing
- [ ] M.9 Consolidation tests

## Feature: Dynamic Integrations via MCP

Goal: Add service integrations conversationally using MCP servers.
Spec: `docs/pfar-feature-dynamic-integrations.md`

- [x] MCP.1 ToolRegistry mutability — RwLock + Arc<dyn Tool> + unregister()
- [x] MCP.2 MCP client — JSON-RPC 2.0 over stdio (initialize, tools/list, tools/call) (15 tests)
- [x] MCP.3 McpTool — Tool trait adapter routing execute() to MCP server (3 tests)
- [x] MCP.4 MCP config types + known server registry (5 entries) + infer_semantics (10 tests)
- [x] MCP.5 McpServerManager — spawn, stop, shutdown lifecycle (5 tests)
- [x] MCP.6 Admin tool MCP actions (connect, disconnect, list_mcp_servers)
- [x] MCP.7 Startup wiring + graceful shutdown integration
- [x] MCP.8 SecurityLabel::FromStr + Display (5 tests)
- [x] MCP.9 Quality assurance (fmt, clippy, all tests green)

## Feature: Credential Acquisition (In-Chat Paste) — SUPERSEDED by KernelFlowManager

Goal: Intercept credential messages before the LLM pipeline (Invariant B).
Spec: `docs/pfar-credential-acquisition.md` (Method 3 only)
**Note**: CredentialGate and deterministic admin plan replaced by KernelFlowManager (see below).

- [x] CG.1 Journal: pending_credential_prompts + pending_message_deletions tables (5 tests) — kept, used by FlowManager
- [x] CG.2 Telegram adapter: DeleteMessage variant + delete_message API call (1 test) — kept
- [x] ~~CG.3 CredentialGate module~~ — absorbed into KernelFlowManager
- [x] ~~CG.4 Pipeline: CredentialPromptInfo in PipelineOutput~~ — removed, FlowManager bypasses pipeline
- [x] CG.5 KnownServer: expected_prefix field + prompt_credential output update — kept
- [x] ~~CG.6 main.rs: wire gate into event loop~~ — replaced by FlowManager wiring
- [x] CG.7 Fix store_credential label ceiling Secret→Sensitive — kept
- [x] ~~CG.8 Quality assurance~~ — superseded by KFM.4

## Feature: KernelFlowManager (Integration Setup State Machine)

Goal: Replace CredentialGate + deterministic admin plan with auto-continuing state machine.
Spec: `docs/pfar-feature-dynamic-integrations-final.md` (KernelFlowManager)

- [x] KFM.1 KernelFlowManager module: FlowState, KernelFlow, start_setup, intercept, advance (15 tests)
- [x] KFM.2 Pipeline cleanup: remove vault field, build_admin_plan, check_service_credential, CredentialPromptInfo
- [x] KFM.3 main.rs: wire FlowManager, parse_connect_command, replace CredentialGate
- [x] KFM.4 Quality assurance (fmt, clippy, all tests green — 435 tests)

## Feature: System Identity Document (SID)

Goal: Dynamically assembled runtime context injected into every LLM prompt.
Spec: `docs/pfar-system-identity-document.md`

- [x] SID.1 ToolRegistry::tool_summaries() method (1 test)
- [x] SID.2 kernel/sid.rs module: build_sid(), render(), IntegrationSummary, ToolSummary (6 tests)
- [x] SID.3 kernel/mod.rs: add pub mod sid
- [x] SID.4 Planner: add sid field to PlannerContext, prepend SID in compose_prompt (2 tests)
- [x] SID.5 Synthesizer: add sid field to SynthesizerContext, prepend SID, persona dedup (1 test)
- [x] SID.6 Pipeline: add sid field, wire through run/execute_full_pipeline/build_planner_context
- [x] SID.7 main.rs: rebuild_sid() at startup and after MCP state changes
- [x] SID.8 Update existing tests for new signatures (~30 test contexts updated)
- [x] SID.9 Quality assurance (fmt, clippy, all tests green — 462 tests)

## Feature: Find Local Skills

Goal: Scan skills directory at startup, parse skill.toml manifests, spawn as MCP servers.
Spec: `docs/pfar-feature-self-extending-skills.md` §3, §14

- [x] SK.1 SkillConfig struct + find_local_skills() in src/tools/mcp/skills.rs (10 tests)
- [x] SK.2 skills_dir config field in McpConfig
- [x] SK.3 Startup wiring: scan + spawn skills in main.rs
- [x] SK.4 admin.list_skills action in AdminTool
- [x] SK.5 Quality assurance (fmt, clippy, all tests green — 483 tests)

## Phase 3: Admin Tool + More Tools + Browser (weeks 6-7)

Goal: Conversational config, browser service, richer tool ecosystem.

- [x] 3.1 Admin tool module (integration management, credential prompts, schedule management)
- [x] 3.2 Credential prompt flow (simplified to multi-turn via session working memory; deferred: delete_credential, rotate_credential)
- [ ] 3.3 GitHub tool (list_prs, get_issue)
- [ ] 3.4 Notion tool (read_page, create_page, query_db)
- [ ] 3.5 Generic HTTP tool
- [ ] 3.6 Email extractor (structured)
- [ ] 3.7 Web page extractor (Readability)
- [ ] 3.8 Browser service (Podman container, leased sessions)
- [ ] 3.9 Script runner (Podman container)
- [ ] 3.10 Container manager + reconciliation loop
- [ ] 3.11 Regression tests: 3, 6, 10, 11, 12 (test 15 done with admin tool)

## Phase 4: Remaining Adapters + Cron + Production (weeks 8-10)

Goal: Full multi-channel, scheduled automation, production readiness.

- [ ] 4.1 Webhook adapter (HMAC + replay protection)
- [ ] 4.2 Cron scheduler (explicit template per job)
- [ ] 4.3 Slack adapter (Socket Mode)
- [ ] 4.4 WhatsApp adapter (Baileys subprocess)
- [ ] 4.5 Remaining tools: Bluesky, Twitter, Fireflies, Cloudflare, Moltbook
- [ ] 4.6 Transcript extractor, health data extractor
- [x] 4.7 Cloud LLM routing (label-based, with owner opt-in) — OpenAI + Anthropic providers, multi-provider proxy, env-based config
- [ ] 4.8 Circuit breakers + fallback chains
- [ ] 4.9 Vault backup/restore
- [ ] 4.10 OpenTelemetry integration
- [ ] 4.11 Memory consolidation cron job
- [ ] 4.12 Error recovery UX (user-friendly failure messages)
- [ ] 4.13 All remaining regression tests: 14
- [ ] 4.14 Security review of Policy Engine paths
- [ ] 4.15 Load test (50 concurrent tasks)
