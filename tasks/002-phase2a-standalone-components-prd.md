# Phase 2a: Standalone Components PRD

## Overview

Implement the standalone agent components that do not depend on each other and can be built in parallel: budget tracker, policy gate, approval manager, Telegram input guard, and UI formatting.

## Goals

- Atomic budget tracking (session + daily) with lock-free counters
- Policy gate with SSRF protection and rate limiting
- Non-blocking approval manager with short base62 IDs
- Inbound credential scanning for Telegram messages
- HTML formatting and inline keyboard helpers

## Tasks

- [x] 1.0 Add Clone derive to BudgetConfig in config.rs
- [x] 2.0 Extract shared credential patterns from executor/redactor.rs
- [x] 3.0 Create src/agent/mod.rs with TelegramOutbound struct
- [x] 4.0 Create src/agent/budget.rs with DailyBudget and SessionBudget
- [x] 5.0 Create src/agent/policy.rs with PolicyDecision, SSRF filter, RateLimiter
- [x] 6.0 Create src/agent/approval.rs with ApprovalManager
- [x] 7.0 Create src/telegram/mod.rs module root
- [x] 8.0 Create src/telegram/input_guard.rs with scan_message
- [x] 9.0 Create src/telegram/ui.rs with HTML formatting helpers
- [x] 10.0 Update src/lib.rs to add agent and telegram modules
- [x] 11.0 Create tests/agent.rs test harness
- [x] 12.0 Create tests/agent/budget_test.rs (6 tests)
- [x] 13.0 Create tests/agent/policy_test.rs (15 tests)
- [x] 14.0 Create tests/agent/approval_test.rs (9 tests)
- [x] 15.0 Create tests/telegram.rs test harness
- [x] 16.0 Create tests/telegram/input_guard_test.rs (6 tests)
- [x] 17.0 Create tests/telegram/ui_test.rs (4 tests)
- [x] 18.0 All 188 tests pass (45 new + 143 existing)
- [x] 19.0 cargo clippy passes with -D warnings
- [x] 20.0 cargo fmt --check passes

## Relevant Files

### Source (new)
- `src/agent/mod.rs` - Module root + TelegramOutbound
- `src/agent/budget.rs` - DailyBudget, SessionBudget, BudgetError
- `src/agent/policy.rs` - PolicyDecision, PolicyContext, is_private_ip, ssrf_check, RateLimiter
- `src/agent/approval.rs` - ApprovalManager, PendingApproval, ApprovalResult
- `src/telegram/mod.rs` - Module root
- `src/telegram/input_guard.rs` - GuardAction, scan_message
- `src/telegram/ui.rs` - escape_html, approval_keyboard, format_tool_call, format_budget

### Source (modified)
- `src/lib.rs` - Added agent and telegram modules
- `src/config.rs` - Added Clone to BudgetConfig
- `src/executor/redactor.rs` - Extracted default_credential_patterns as pub fn

### Tests (new)
- `tests/agent.rs` - Test harness
- `tests/agent/budget_test.rs` - 6 tests
- `tests/agent/policy_test.rs` - 15 tests
- `tests/agent/approval_test.rs` - 9 tests
- `tests/telegram.rs` - Test harness
- `tests/telegram/input_guard_test.rs` - 6 tests
- `tests/telegram/ui_test.rs` - 4 tests
