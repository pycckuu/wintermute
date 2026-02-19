---
name: quality-check-wintermute
description: Automated quality gate for Wintermute. Runs build, tests, clippy, formatting, doc coverage, security invariant grep scans, test placement policy checks, and dependency audit. Reports pass/fail for each check with actionable fixes.
---

# Wintermute Quality Check Workflow

## Role

You are a **Quality Gate Enforcer** for the Wintermute project. Run all checks sequentially, collect results, and present a clear pass/fail summary. Fix issues when possible, report when not.

## Checks (run in order)

### 1. Build Check

```bash
cargo build --all-targets 2>&1
```

- **PASS**: Exit code 0, no errors
- **FAIL**: Report compilation errors with file paths and line numbers

### 2. Test Check

```bash
cargo test --all-features 2>&1
```

- **PASS**: All tests pass
- **FAIL**: Report failing test names, assertion messages, and file locations

### 3. Clippy Check

```bash
cargo clippy --all-targets --all-features -- -D warnings 2>&1
```

- **PASS**: No warnings or errors
- **FAIL**: Report each warning with file path, line, and suggestion

### 4. Format Check

```bash
cargo fmt --check 2>&1
```

- **PASS**: All files formatted correctly
- **FAIL**: List files that need formatting. Offer to run `cargo fmt` to fix.

### 5. Doc Coverage Check

```bash
cargo doc --no-deps 2>&1
```

- **PASS**: No missing-docs warnings
- **FAIL**: List public items missing `///` doc comments with file paths

### 6. Security Invariant Scan

Fast static grep checks for common invariant violations:

**Invariant 1 — No host executor:**
```bash
rg "std::process::Command|tokio::process::Command" src/ --type rust --glob '!**/test*' --glob '!**/mock*'
```
- **PASS**: No matches (only DockerExecutor should execute commands)
- **FAIL**: List each occurrence

**Code Rule — No unwrap():**
```bash
rg "\.unwrap\(\)" src/ --type rust --glob '!**/test*' --glob '!**/mock*'
```
- **PASS**: No matches outside test code
- **FAIL**: List each occurrence — use `?`, `.context()`, or `.ok_or_else()`

**Code Rule — No println/eprintln:**
```bash
rg "println!|eprintln!" src/ --type rust --glob '!**/test*' --glob '!**/mock*'
```
- **PASS**: No matches outside test code
- **FAIL**: List each occurrence — use `tracing` macros instead

**Code Rule — No unsafe:**
```bash
rg "unsafe " src/ --type rust
```
- **PASS**: No matches
- **FAIL**: List each occurrence — `#![forbid(unsafe_code)]` should prevent this

**Test Placement Policy — No tests in src/:**
```bash
rg "#\\[cfg\\(test\\)\\]" src/ --type rust
```
- **PASS**: No matches
- **FAIL**: List each occurrence — all tests must be moved to mirrored paths under `tests/`

### 7. Dependency Audit

```bash
cargo deny check 2>&1 || echo "cargo-deny not configured, skipping"
```

- **PASS**: No advisories or license violations
- **FAIL**: Report advisories with severity and affected crates
- **SKIP**: If cargo-deny is not installed

## Output Format

Present results as a summary table:

```
## Quality Gate Results

| # | Check              | Status | Details                        |
|---|-------------------|--------|--------------------------------|
| 1 | Build             | PASS   |                                |
| 2 | Tests             | PASS   | 42 tests passed                |
| 3 | Clippy            | FAIL   | 3 warnings (see below)         |
| 4 | Format            | PASS   |                                |
| 5 | Doc Coverage      | FAIL   | 2 public items missing docs    |
| 6 | Security Scan     | PASS   |                                |
| 7 | Dependency Audit  | SKIP   | cargo-deny not installed       |

## Overall: FAILED (2 checks failed)

### Failures

#### Clippy (3 warnings)
- `src/agent/budget.rs:45` — unnecessary clone
- `src/executor/docker.rs:112` — redundant pattern match
- `src/telegram/ui.rs:28` — unused variable

#### Doc Coverage (2 items)
- `src/agent/policy.rs` — `pub fn check_egress()` missing doc comment
- `src/memory/search.rs` — `pub struct SearchResult` missing doc comment
```

If all checks pass:

```
## Quality Gate: PASSED

All 7 checks passed. Code is ready for review.
```

## After Reporting

- If there are FAIL results, offer to fix the issues automatically where possible (formatting, simple clippy fixes)
- For security invariant violations, explain why the violation matters and suggest the correct pattern
- Do NOT proceed to commit/PR if any check fails — the developer must fix issues first
