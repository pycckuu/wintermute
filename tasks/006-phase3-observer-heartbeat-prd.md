# Phase 3: Intelligence — Observer + Heartbeat

**Branch:** `pycckuu/phase3-observer-heartbeat`
**Status:** COMPLETE

---

## Summary

Phase 3 implements two background intelligence subsystems:
- **Task 7: Observer** — learns from conversations by extracting facts/procedures and staging them for promotion to active memory
- **Task 8: Heartbeat** — scheduled task execution, backup automation, health monitoring

## New Files

| File | Purpose |
|------|---------|
| `src/observer/mod.rs` | Observer pipeline entry: receives idle events, dispatches extraction + staging |
| `src/observer/extractor.rs` | LLM-based fact/procedure extraction from conversation snapshots |
| `src/observer/staging.rs` | Staged promotion: pending -> active, duplicate/contradiction detection, undo |
| `src/heartbeat/mod.rs` | Heartbeat runner: tokio interval loop, dispatches scheduled tasks + health |
| `src/heartbeat/scheduler.rs` | Cron evaluation, builtin + dynamic tool task dispatch |
| `src/heartbeat/backup.rs` | Backup via recursive copy (scripts) + VACUUM INTO (memory DB) |
| `src/heartbeat/health.rs` | Health report collection + atomic JSON file writing |

## Modified Files

| File | Changes |
|------|---------|
| `src/lib.rs` | Uncommented `pub mod observer` and `pub mod heartbeat` |
| `src/config.rs` | Added `Clone` derive to `LearningConfig` |
| `src/agent/mod.rs` | Added `observer_tx` to `SessionRouter` |
| `src/agent/loop.rs` | Added 2-min idle detection, observer event sending |
| `src/agent/budget.rs` | Added `pub fn limit(&self) -> u64` |
| `src/memory/mod.rs` | Added `search_by_status`, `delete_memory`, `count_by_status`, `db_size_bytes` |
| `src/memory/search.rs` | Added `search_by_status` SQL query |
| `src/memory/writer.rs` | Added `WriteOp::DeleteMemory` + handler |
| `src/telegram/commands.rs` | Replaced stubs with real implementations for `/memory_pending`, `/memory_undo`, `/backup` |
| `src/telegram/mod.rs` | Added `paths` and `memory_pool` to `SharedState`, updated `run_telegram` |
| `src/main.rs` | Wired observer channel + spawn, heartbeat spawn with shutdown watch |

## Test Files

| File | Tests |
|------|-------|
| `tests/observer.rs` | Entrypoint |
| `tests/observer/extractor_test.rs` | 8 tests: JSON parsing, confidence filter, serialization |
| `tests/observer/staging_test.rs` | 5 tests: staging, duplicates, promotions, undo |
| `tests/heartbeat.rs` | Entrypoint |
| `tests/heartbeat/scheduler_test.rs` | 7 tests: cron matching, disabled, last_run, multiple schedules |
| `tests/heartbeat/backup_test.rs` | 3 tests: timestamped dir, missing scripts, nested copy |
| `tests/heartbeat/health_test.rs` | 4 tests: serialization, error state, file write, atomic overwrite |

## Security Invariants Preserved

1. No `process::Command` — backup uses `VACUUM INTO` (pure SQL) + recursive copy
2. Budget checked before every observer LLM call
3. Observer LLM output passes through Redactor before parsing
4. SQL injection prevented in `VACUUM INTO` via path validation (no single quotes)
5. Egress controlled — observer LLM calls go through ModelRouter (same path as agent loop)

## QA Results

### Code Review Findings (Fixed)

| Severity | Issue | Fix |
|----------|-------|-----|
| CRITICAL | SQL injection in `vacuum_into` via unsanitized path | Added single-quote validation before interpolation |
| HIGH | Auto-promotion double-counting in `staging.rs` | Track promoted IDs in HashSet |
| HIGH | `user_id: 0` in heartbeat notifications | Added `notify_user_id` field to HeartbeatDeps, sourced from first allowed_user |
| HIGH | Dead code branch in `word_overlap` (union==0 unreachable) | Removed dead branch with explaining comment |
| MEDIUM | `unwrap_or_default()` silencing DB errors | Propagated with `?` via `.context()` |
| MEDIUM | Unnecessary `.clone()` of messages vec | Changed to move when not truncating |
| MEDIUM | Fallback to `now` in scheduler skips first-run tasks | Changed to `UNIX_EPOCH` so first cron match triggers |

### Final Verification

- `cargo build --release`: PASS
- `cargo test --all-features`: 350 tests, 0 failures
- `cargo clippy --all-targets --all-features -- -D warnings`: CLEAN
- `cargo fmt --check`: CLEAN

## Tasks

- [x] 1. Extend MemoryEngine with status queries
- [x] 2. Observer module: types and pipeline (`src/observer/mod.rs`)
- [x] 3. Observer extractor (`src/observer/extractor.rs`)
- [x] 4. Observer staging (`src/observer/staging.rs`)
- [x] 5. Agent loop: idle detection + observer channel
- [x] 6. Heartbeat module: tick loop (`src/heartbeat/mod.rs`)
- [x] 7. Heartbeat scheduler (`src/heartbeat/scheduler.rs`)
- [x] 8. Heartbeat backup (`src/heartbeat/backup.rs`)
- [x] 9. Heartbeat health (`src/heartbeat/health.rs`)
- [x] 10. Update Telegram command stubs with real implementations
- [x] 11. Wire in main.rs (observer + heartbeat spawning)
- [x] 12. Uncomment modules in lib.rs
- [x] 13. Code review + fix all findings
- [x] 14. Formatting + clippy clean
