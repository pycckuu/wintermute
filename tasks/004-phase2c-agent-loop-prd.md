# Phase 2c: Agent Loop

## Overview
Implement the core agent reasoning loop, context assembly, and session routing for Wintermute.

## Tasks

- [x] 1.0 Create `src/agent/context.rs` — system prompt assembly and message trimming
- [x] 2.0 Create `src/agent/loop.rs` — core reasoning cycle (SessionEvent, run_session, run_agent_turn)
- [x] 3.0 Modify `src/agent/mod.rs` — add SessionRouter and re-exports
- [x] 4.0 Create `tests/agent/context_test.rs` — context assembly and trimming tests (9 tests)
- [x] 5.0 Create `tests/agent/loop_test.rs` — session event and agent turn tests (7 tests)
- [x] 6.0 Create `tests/agent/session_test.rs` — session router tests (5 tests)
- [x] 7.0 Update `tests/agent.rs` — register new test modules
- [x] 8.0 Verify all quality checks pass (cargo fmt, clippy, test — 197 total tests)
