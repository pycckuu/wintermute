# PFAR v2 — Implementation Tasks

> Based on spec: `docs/pfar-v2-spec.md` Section 19

## Progress

- [x] Phase 0: Repository setup from template
- [x] Phase 1: Kernel Core (73 tests passing — 66 unit + 7 integration)
- [x] Phase 2: Telegram + Pipeline + First Tools (274 tests — 248 unit + 7 integration + 19 regression)
- [x] Feature: Persistence & Recovery (364 tests — 326 unit + 7 integration + 31 regression)

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

## Feature: Persistence & Recovery

Goal: SQLite-backed task journaling, recovery logic, graceful shutdown, adapter state persistence.
Spec: `docs/pfar-feature-persistence-recovery.md`

- [x] P.1 TaskJournal module with SQLite schema (30 tests)
- [x] P.2 Integrate journal writes into Pipeline (6 tests)
- [x] P.3 Executor step-by-step journal writes (3 tests)
- [x] P.4 Recovery logic module (12 tests)
- [x] P.5 Telegram adapter state persistence (4 tests)
- [x] P.6 Graceful shutdown — audit events + signal handling (2 tests)
- [x] P.7 Startup integration and owner notification — journal wiring, recovery, cleanup
- [x] P.8 Regression tests R1-R12 (12 tests)
- [x] P.9 Session persistence — conversation history + working memory survive restarts (15 tests)

## Phase 3: Admin Tool + More Tools + Browser (weeks 6-7)

Goal: Conversational config, browser service, richer tool ecosystem.

- [ ] 3.1 Admin tool module (integration management, credential prompts, schedule management)
- [ ] 3.2 Credential prompt flow (task suspension + resume)
- [ ] 3.3 GitHub tool (list_prs, get_issue)
- [ ] 3.4 Notion tool (read_page, create_page, query_db)
- [ ] 3.5 Generic HTTP tool
- [ ] 3.6 Email extractor (structured)
- [ ] 3.7 Web page extractor (Readability)
- [ ] 3.8 Browser service (Podman container, leased sessions)
- [ ] 3.9 Script runner (Podman container)
- [ ] 3.10 Container manager + reconciliation loop
- [ ] 3.11 Regression tests: 3, 6, 10, 11, 12, 15

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
