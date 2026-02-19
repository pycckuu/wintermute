# Phase 2d: Telegram Adapter + Commands + main.rs Wiring

## Introduction/Overview

Wire the Telegram bot adapter to connect inbound messages, slash commands, callback queries, and outbound agent responses. This completes the Telegram integration by adding command handlers and updating `main.rs` to instantiate and run the full bot.

## Goals

- Implement slash command handlers for `/help`, `/status`, `/budget`, `/memory`, `/tools`, `/sandbox`, `/backup`
- Build the Telegram adapter using teloxide v0.13 with dptree dispatcher
- Wire all Phase 2 components together in `handle_start()`
- Maintain security invariants (credential scanning, allowed_users checks)

## Functional Requirements

1. Slash commands respond with HTML-formatted messages
2. Only allowed_users can interact with the bot
3. Inbound messages are scanned for credentials before routing
4. Callback queries handle approval keyboard responses
5. Outbound messages support text, files, and inline keyboards

## Non-Goals

- Observer pipeline (Phase 3)
- Heartbeat system (Phase 3)
- Browser tool integration

## Tasks

- [x] 1.0 Create `src/telegram/commands.rs` with slash command handlers
- [x] 2.0 Update `src/telegram/mod.rs` with adapter function and SharedState
- [x] 3.0 Update `src/main.rs` to wire Phase 2 components in `handle_start()`
- [x] 4.0 Create `tests/telegram/commands_test.rs` with command handler tests
- [x] 5.0 Update `tests/telegram.rs` to include commands_test module
- [x] 6.0 Verify cargo build and tests pass

## Relevant Files

- `src/telegram/mod.rs` — adapter entry point
- `src/telegram/commands.rs` — slash command handlers (new)
- `src/telegram/input_guard.rs` — credential scanning
- `src/telegram/ui.rs` — HTML formatting helpers
- `src/agent/mod.rs` — SessionRouter, TelegramOutbound
- `src/agent/approval.rs` — ApprovalManager
- `src/config.rs` — Config, TelegramConfig
- `src/executor/mod.rs` — Executor trait
- `src/memory/mod.rs` — MemoryEngine
- `src/tools/registry.rs` — DynamicToolRegistry
- `src/main.rs` — CLI entry point
- `tests/telegram.rs` — test entry point
- `tests/telegram/commands_test.rs` — command tests (new)
