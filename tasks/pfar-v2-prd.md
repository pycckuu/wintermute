# PFAR v2 — Implementation Tasks

> Based on spec: `docs/pfar-v2-spec.md` Section 19

## Progress

- [x] Phase 0: Repository setup from template

## Phase 1: Kernel Core (weeks 1-3)

Goal: Working kernel that receives events, matches templates, enforces policies.

- [ ] 1.1 Core types: Principal, SecurityLabel, TaintSet, CapabilityToken, Task
- [ ] 1.2 Event router (accepts test events via CLI adapter)
- [ ] 1.3 Principal resolution
- [ ] 1.4 Policy Engine: label assignment, propagation, No Read Up, No Write Down
- [ ] 1.5 Policy Engine: taint checking (graduated rules)
- [ ] 1.6 Policy Engine: capability token generation and validation
- [ ] 1.7 Task template engine (load TOML, match triggers, validate plans)
- [ ] 1.8 Vault abstraction (OS keychain for master key, SQLCipher for three DBs)
- [ ] 1.9 Inference proxy (HTTP to Ollama on localhost)
- [ ] 1.10 Audit logger (structured JSON)
- [ ] 1.11 Unit tests for all Policy Engine functions
- [ ] 1.12 Integration test: CLI event -> template match -> mock plan -> mock execute -> response

## Phase 2: Telegram + Pipeline + First Tools (weeks 4-5)

Goal: End-to-end flow: Telegram message -> extract -> plan -> execute -> synthesize -> reply.

- [ ] 2.1 Telegram adapter (in-process async task, polling)
- [ ] 2.2 Phase 0: Message intent extractor (simple classifier)
- [ ] 2.3 Phase 1: Planner (LLM via inference proxy)
- [ ] 2.4 Phase 2: Kernel plan executor (in-process tool dispatch)
- [ ] 2.5 Phase 3: Synthesizer (LLM via inference proxy)
- [ ] 2.6 Egress validation and message delivery
- [ ] 2.7 Session working memory (per-principal, in vault)
- [ ] 2.8 Conversation history (sliding window)
- [ ] 2.9 Two read-only tools: calendar.freebusy, email.list + email.read
- [ ] 2.10 ScopedHttpClient with domain allowlist + private IP blocking
- [ ] 2.11 Approval queue (Telegram inline buttons) for tainted writes
- [ ] 2.12 Regression tests: 1, 2, 4, 5, 7, 8, 9, 13, 16, 17

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
- [ ] 4.7 Cloud LLM routing (label-based, with owner opt-in)
- [ ] 4.8 Circuit breakers + fallback chains
- [ ] 4.9 Vault backup/restore
- [ ] 4.10 OpenTelemetry integration
- [ ] 4.11 Memory consolidation cron job
- [ ] 4.12 Error recovery UX (user-friendly failure messages)
- [ ] 4.13 All remaining regression tests: 14
- [ ] 4.14 Security review of Policy Engine paths
- [ ] 4.15 Load test (50 concurrent tasks)
