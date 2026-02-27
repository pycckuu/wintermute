# 010: Messaging, Session Persistence & Outbound Privacy

## Status: Implementation Complete, QA Passed

## Overview

Wintermute can now act as a personal fixer: managing real-world tasks through WhatsApp with scoped privacy boundaries, session persistence, and autonomous multi-turn conversations.

## Task Progress

### Phase 1: Session Persistence [DONE]
- [x] Migration 003_sessions.sql
- [x] SessionManager (create, checkpoint, recover, complete)
- [x] SessionsConfig in agent.toml
- [x] Crash recovery on startup
- [x] Session checkpoint after every agent turn

### Phase 2: Outbound Privacy & Task Briefs [DONE]
- [x] Migration 004_briefs.sql (task_briefs, contacts, outbound_log)
- [x] TaskBrief, Constraint, CommitmentLevel, BriefStatus types
- [x] Brief lifecycle with state machine validation
- [x] OutboundComposer (restricted-context LLM composition)
- [x] OutboundRedactor (privacy pattern scanning)
- [x] Outbound context isolation (brief-only, no USER.md/memories/AGENTS.md)
- [x] Audit logging for all outbound messages
- [x] send_message tool (replaces send_telegram)
- [x] manage_brief tool (create/update/escalate/propose/complete/cancel)
- [x] read_messages tool (stub for Phase 3)
- [x] Contact resolution and persistence
- [x] PrivacyConfig extensions (never_share, private_terms, require_brief_confirmation)
- [x] MessagingConfig in agent.toml

### Phase 3: WhatsApp Adapter [DONE]
- [x] WhatsAppClient HTTP bridge (health, QR, send, messages, contacts, mark_read)
- [x] Event listener with long-polling and exponential backoff
- [x] Docker container lifecycle (ensure_container, setup_qr) via bollard
- [x] Message routing by JID → contact → active brief
- [x] WhatsAppConfig in config.toml
- [x] Sidecar detection in startup

### Phase 4: Autonomous Conversations [DONE]
- [x] SessionEvent::InboundMessage for WhatsApp inbound routing
- [x] InboundMessage handler in run_session (brief load, context inject, agent turn)
- [x] SessionRouter::route_inbound() for session dispatch
- [x] Active briefs injected into system prompt (Section 6.5)
- [x] load_conversation_history() for multi-turn outbound threading
- [x] Active briefs in proactive heartbeat context
- [x] WhatsApp event listener wired in main.rs with routing

### Phase 5: Polish [DONE]
- [x] Human-like timing (2-15s delay based on message lengths)
- [x] Typing indicators (send_typing)
- [x] Read receipts (mark_read)
- [x] Full WhatsApp send flow in send_message tool (brief → compose → redact → delay → type → send → audit)
- [x] WhatsAppClient and OutboundComposer wired into ToolRouter

### QA Pipeline [DONE]
- [x] Security invariant quick checks (no host exec, no container env, redactor chokepoint)
- [x] Test layout verification (no #[cfg(test)] in src/)
- [x] Doc coverage check
- [x] Code refactorer (extracted duplicated conversions, checkpoint helper, fixed unsafe casts)
- [x] Security review (fixed: string slice panic, inbound audit logging, session_id trust boundary, documented network/writer exceptions)
- [x] Linter pass (clippy + fmt clean)
- [x] Final build/test/clippy (111 tests pass)

## New Files

| File | Purpose |
|------|---------|
| `migrations/003_sessions.sql` | Sessions table for crash recovery |
| `migrations/004_briefs.sql` | Task briefs, contacts, outbound log tables |
| `src/agent/session_manager.rs` | Session persistence and recovery |
| `src/messaging/mod.rs` | Messaging error types |
| `src/messaging/brief.rs` | TaskBrief lifecycle and persistence |
| `src/messaging/outbound_composer.rs` | Restricted-context message composition |
| `src/messaging/outbound_redactor.rs` | Privacy pattern scanning |
| `src/messaging/outbound_context.rs` | Brief-only system prompt builder |
| `src/messaging/contacts.rs` | Contact resolution and persistence |
| `src/messaging/audit.rs` | Outbound audit logging |
| `src/whatsapp/mod.rs` | WhatsApp error types |
| `src/whatsapp/client.rs` | HTTP bridge client |
| `src/whatsapp/events.rs` | Long-polling event listener |
| `src/whatsapp/setup.rs` | Docker container lifecycle |
| `src/whatsapp/router.rs` | Inbound message routing |
| `src/tools/send_message.rs` | Unified send_message (Telegram + WhatsApp) |
| `src/tools/manage_brief.rs` | Brief CRUD operations |
| `src/tools/read_messages.rs` | WhatsApp message reading |

## Security Invariants

| # | Invariant | Status |
|---|-----------|--------|
| 1 | No host executor | PASS — no std::process::Command |
| 2 | Container env empty | PASS — no secrets in containers |
| 3 | No container network | PASS — WhatsApp sidecar on own network |
| 4 | Egress controlled | PASS — brief gate + redactor + rate limits + audit |
| 5 | Budget atomic | PASS — DailyBudget::check() before compose |
| 6 | Credential scanning | PASS — baileys auth never read by Rust |
| 7 | Redactor chokepoint | PASS — OutboundRedactor.scan() before send |
| 8 | Config split | PASS — WhatsApp in config.toml, messaging in agent.toml |
