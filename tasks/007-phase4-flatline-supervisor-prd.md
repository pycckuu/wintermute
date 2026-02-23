# Phase 4: Flatline Supervisor — PRD

## Overview

Flatline is the supervisor process for Wintermute. A separate binary that watches,
diagnoses, and heals — without interfering with the working agent. Named after
Dixie Flatline from Neuromancer.

## Implementation Progress

### Foundation (Phase 1)

- [x] 1.1 `flatline/Cargo.toml` — full dependencies and lint configuration
- [x] 1.2 `flatline/src/lib.rs` — crate root with module declarations
- [x] 1.3 `flatline/src/config.rs` — configuration loading with serde defaults
- [x] 1.4 `flatline/src/db.rs` — state database (SQLite) with migration
- [x] 1.5 `flatline/migrations/001_flatline_schema.sql` — schema migration
- [x] 1.6 `flatline/src/watcher.rs` — log tailing + health file monitoring
- [x] 1.7 `flatline/src/stats.rs` — rolling statistics engine
- [x] 1.8 `flatline/src/main.rs` — CLI scaffold (start + check subcommands)
- [x] 1.9 `flatline.toml.example` — configuration template
- [x] 1.10 Stub modules: `diagnosis.rs`, `fixer.rs`, `patterns.rs`, `reporter.rs`

### Tests (Phase 1)

- [x] 1.11 `flatline/tests/config_test.rs` — 5 tests
- [x] 1.12 `flatline/tests/db_test.rs` — 9 tests
- [x] 1.13 `flatline/tests/watcher_test.rs` — 8 tests
- [x] 1.14 `flatline/tests/stats_test.rs` — 7 tests

### Verification

- [x] `cargo build --workspace --all-targets` passes
- [x] `cargo test --workspace --all-features` passes (450 tests)
- [x] `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes
- [x] `cargo fmt --all --check` passes

### Patterns + Fixes (Phase 2)

- [x] 2.1 `flatline/src/patterns.rs` — 8 known failure patterns (21 tests)
- [x] 2.2 `flatline/src/fixer.rs` — fix lifecycle (propose/apply/verify) (25 tests)
- [x] 2.3 `flatline/tests/patterns_test.rs` — 21 tests
- [x] 2.4 `flatline/tests/fixer_test.rs` — 25 tests

### Reporting + LLM (Phase 3)

- [x] 3.1 `flatline/src/reporter.rs` — Telegram notifications (cooldown, HTML formatting, daily health)
- [x] 3.2 `flatline/src/diagnosis.rs` — LLM diagnosis (budget check, evidence building, JSON parsing)
- [x] 3.3 Expand `main.rs` with full daemon loop + check command
- [x] 3.4 `flatline/tests/reporter_test.rs` — 8 tests (cooldown tracking, construction)
- [x] 3.5 `flatline/tests/diagnosis_test.rs` — 10 tests (JSON parsing, serde roundtrip, edge cases)

### Hardening (Phase 4)

- [x] 4.1 Restart rate-limiting (max_auto_restarts_per_hour enforced in daemon loop)
- [x] 4.2 Alert cooldowns (per-pattern cooldown tracking in Reporter)
- [x] 4.3 Config validation (bounds checking on all numeric thresholds)
- [x] 4.4 `flatline/tests/security_test.rs` — 7 security invariant tests
- [x] 4.5 Security review findings fixed (11 findings: PID validation, task/tool name validation, symlink safety, line length limits, log extension filter, spawn_blocking for process commands, git hash validation, send_to_all failure tracking)

### Quality Assurance

- [x] Code refactoring pass (extracted helpers, reduced duplication, improved naming)
- [x] Security review (1 CRITICAL, 3 HIGH, 4 MEDIUM, 3 LOW — all fixed)
- [x] Linter pass (cargo fmt + clippy clean)
- [x] Test layout verified (no `#[cfg(test)]` in src/)
- [x] Doc coverage verified (no flatline warnings)
- [x] Final build/test/clippy all green (450 tests, 0 failures)

### Documentation

- [x] DESIGN.md updated (Flatline reference updated)
- [x] dev/AGENT.md updated (flatline scope + project structure)
- [x] PRD updated with progress
