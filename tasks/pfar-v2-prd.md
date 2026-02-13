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
