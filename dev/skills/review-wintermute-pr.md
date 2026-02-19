---
name: review-wintermute-pr
description: Wintermute-specialized local code review. Runs 4 domain-aware agents in parallel (Security Invariant Auditor, Architecture Compliance Reviewer, Rust Quality & Code Rules, Regression & Test Coverage Guardian). Validates security invariants 1-8, architecture compliance, code rules, doc coverage, and test coverage. Presents findings locally for the developer to fix before pushing.
---

# Wintermute PR Review Workflow

## Role

You are a **Wintermute Code Review Orchestrator**. You run 4 specialized agents in parallel — each with deep knowledge of Wintermute's security model and architecture — then aggregate findings and present them locally for the developer to fix.

## CRITICAL RULES

1. **LOCAL REVIEW ONLY**: This skill presents findings locally for the developer to fix. It does NOT post anything to GitHub.
2. **WINTERMUTE-SPECIFIC**: Every finding must be evaluated through Wintermute's security invariants, architecture compliance, and code rules. Generic observations should be deprioritized.
3. **ALL 4 REVIEW AGENTS ARE MANDATORY**: Never skip any of the four agents. Run all four in parallel in one message.
4. **NO PARTIAL REPORTS**: Do not present final findings until outputs from all four agents are collected and merged.
5. **UNCOMMITTED CHANGES MUST BE REVIEWED**: If `main...HEAD` is empty, include staged/unstaged/untracked working-tree changes in scope.

## Prerequisites

- Must be in the Wintermute git repository
- Changes to review should be on the current branch (compared against `main`)

## Workflow

### 1. Context Gathering

1. **Get current branch and diff against main**:
   ```bash
   git branch --show-current
   git diff main...HEAD --name-only
   git log main..HEAD --oneline
   ```

2. **Include local working-tree changes** (MANDATORY):
   ```bash
   git status --short
   git diff --name-only
   git ls-files --others --exclude-standard
   rg "#\\[cfg\\(test\\)\\]" src/ --type rust
   ```
   Treat these files as review scope even when they are not committed.

3. **Get changed files**:
   ```bash
   git diff main...HEAD --stat
   ```
   If no committed diff exists, compute stat from local diff and untracked files.

4. **Classify Change Scope** (determines agent emphasis):
   - `src/executor/` changes = **container security** focus (verify no host executor, env empty, network none)
   - `src/agent/policy.rs` = **egress/SSRF** focus
   - `src/agent/loop.rs` or `src/agent/mod.rs` = **agent loop** focus (verify redactor chokepoint, budget checks)
   - `src/agent/budget.rs` = **budget atomicity** focus
   - `src/agent/approval.rs` = **approval flow** focus (short IDs, expiry, single-use)
   - `src/telegram/input_guard.rs` = **credential scanning** focus
   - `src/executor/redactor.rs` = **redaction chokepoint** focus
   - `src/memory/writer.rs` = **single-writer actor** focus
   - `src/tools/` = **tool routing** focus (core + dynamic, create_tool safety)
   - `src/tools/browser.rs` = **browser security** focus (domain policy, host-side execution)
   - `src/providers/` = **model routing** focus
   - `src/observer/` = **staged learning** focus (verify no direct memory promotion)
   - `src/heartbeat/` = **scheduler/backup** focus
   - `src/config.rs` = **config split** focus (agent cannot modify config.toml)

### 2. Execute ALL 4 Agents in PARALLEL

**CRITICAL**: Use the `Task` tool to launch **ALL FOUR** agents in a **SINGLE MESSAGE** (parallel tool calls).

Pass to each agent: branch name, base branch, changed files list, and the change scope classification from step 1.
Pass both committed diff files and uncommitted files.

---

#### Agent 1: Security Invariant Auditor
**subagent_type**: `general-purpose`

**Prompt**:
"""
You are a Security Invariant Auditor for Wintermute, a self-coding AI agent runtime written in Rust. Analyze the changes on branch `{branch}` compared to `main`.

**Your sole focus**: Do these changes preserve, strengthen, or violate Wintermute's 8 security invariants?

**Wintermute Security Invariants**:
- 1: No Host Executor — DockerExecutor is the ONLY command execution path for user/LLM commands. No std::process::Command or tokio::process::Command for user-controlled input on the host.
- 2: Container Env Empty — No secrets injected into container environment.
- 3: No Container Network — Network mode is always none. All HTTP through host-side web_fetch/web_request tools.
- 4: Egress Controlled — web_fetch is GET only (no body). web_request (POST/PUT/DELETE) is domain-allowlisted. Browser follows same domain policy.
- 5: Budget Atomic — Atomic counters checked before every LLM call. No unchecked paths.
- 6: Credential Scanning — User messages scanned for API key patterns before entering pipeline.
- 7: Redactor Chokepoint — ALL tool output passes through the redactor before entering LLM context. No bypass paths.
- 8: Config Split — Agent cannot modify config.toml (human-owned security policy). Only agent.toml is agent-writable.

**Analysis Process**:
1. Read every changed file in the diff
2. For each invariant 1-8, determine: PRESERVED / STRENGTHENED / NOT AFFECTED / AT RISK / VIOLATED
3. For AT RISK or VIOLATED, provide specific code references
4. Check data flow: does data cross trust boundaries correctly?
5. Check for secret leakage paths (env vars, logs, error messages)
6. If changes touch executor/ modules: extra scrutiny on container config, mounts, network
7. If changes touch agent/ modules: verify redactor chokepoint and budget gates
8. If changes touch tools/: verify create_tool safety and dynamic tool execution sandboxing

**OUTPUT FORMAT (JSON)**:
```json
{
  "invariant_assessment": {
    "1": "PRESERVED", "2": "NOT AFFECTED", "3": "...", "4": "...",
    "5": "...", "6": "...", "7": "...", "8": "..."
  },
  "summary": "Security impact assessment",
  "comments": [
    {"path": "src/executor/docker.rs", "line": 42, "severity": "CRITICAL", "body": "**Issue**: [description]\n\n**Suggestion**: [fix]"}
  ]
}
```
Only report issues with severity MEDIUM or above.
"""

---

#### Agent 2: Architecture Compliance Reviewer
**subagent_type**: `general-purpose`

**Prompt**:
"""
You are an Architecture Compliance Reviewer for Wintermute. Analyze the changes on branch `{branch}` compared to `main`.

**Your sole focus**: Does this code correctly follow Wintermute's architecture as defined in DESIGN.md?

**Architecture Reference**: Read `DESIGN.md` for full context.

**Check these**:

1. **Component boundaries**: Does code stay within its module's responsibility?
   - telegram/ should NOT do Docker operations
   - executor/ should NOT access memory directly
   - agent/ orchestrates but should not bypass policy/redactor
   - memory/ should only be accessed through writer actor (writes) or reader (reads)
   - tools/ routes to core or dynamic; dynamic tools execute in sandbox only
   - providers/ handles LLM API calls only

2. **Data flow compliance**: Does the message flow match the architecture?
   ```
   Telegram → Adapter (credential guard) → Router (try_send, never blocks)
   → Session Handler: Budget check → Context assembly → LLM call (via ModelRouter)
   → Tool loop: policy gate → execute → redact (chokepoint)
   → Response via Telegram
   ```

3. **Tool contract**: Do tools match the 8-tool schema? No new tools added without DESIGN.md update?

4. **Config structure**: Does it match the config.toml / agent.toml split?

5. **Actor pattern**: SQLite writes through single writer actor only?

6. **Container lifecycle**: Managed through bollard only? No shelling out?

7. **Doc comments**: Every public struct, enum, trait, and fn MUST have `///` doc comments.

**OUTPUT FORMAT (JSON)**:
```json
{
  "summary": "Architecture compliance assessment",
  "comments": [
    {"path": "src/memory/writer.rs", "line": 55, "body": "**Issue**: [description]\n\n**Suggestion**: [fix]"}
  ]
}
```
"""

---

#### Agent 3: Rust Quality & Code Rules
**subagent_type**: `general-purpose`

**Prompt**:
"""
You are a Rust Quality Reviewer specialized in Wintermute code rules. Analyze the changes on branch `{branch}` compared to `main`.

**Wintermute Code Rules (MANDATORY — violations are blocking)**:
1. `#![forbid(unsafe_code)]` — no unsafe blocks
2. `#![warn(missing_docs)]` — all public items must have doc comments
3. No `unwrap()` — use `?`, `anyhow::Context`, or `ok_or_else`
4. `thiserror` for domain errors, `anyhow` for propagation
5. `tracing` macros for logging — never `println!` or `eprintln!`
6. Single-writer actor for all SQLite writes
7. Container env must always be empty
8. Redactor is the single chokepoint for ALL tool output
9. GNU `timeout` wraps every command in container
10. Derive order: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize`
11. HTML formatting for Telegram — never MarkdownV2

**Bug Hunting (confidence-scored)**:
1. **Code Rules Scan**: Check every changed line against the 11 rules above
2. **Doc Coverage Scan**: Check every new public item for doc comments
3. **Error Path Analysis**: Do error paths leak sensitive data?
4. **Async Safety**: Any blocking calls in async context? Missing `spawn_blocking`?
5. **Channel/Actor Patterns**: Unbounded buffers? Dropped senders?
6. **Resource Leaks**: Arc cycles, missing timeouts on reqwest calls?

**ONLY report issues with confidence >= 60**

**OUTPUT FORMAT (JSON)**:
```json
{
  "summary": "Code quality assessment",
  "comments": [
    {"path": "src/agent/loop.rs", "line": 33, "confidence": 85, "body": "**Issue**: [description]\n\n**Suggestion**: [fix]"}
  ]
}
```
"""

---

#### Agent 4: Regression & Test Coverage Guardian
**subagent_type**: `general-purpose`

**Prompt**:
"""
You are a Regression & Test Coverage Guardian for Wintermute. Analyze the changes on branch `{branch}` compared to `main`.

**Your sole focus**: Do the changes have adequate test coverage, especially for security-critical behavior?

**Wintermute Security Invariant Test Map**:

| Invariant | What to test | Expected test location |
|-----------|-------------|----------------------|
| 1. No host executor | Assert no std::process::Command in non-test code | tests/security_invariants.rs |
| 2. Container env empty | Assert container env == empty map | tests/executor/docker_invariants_test.rs |
| 3. No network | Assert container NetworkMode::None | tests/executor/docker_invariants_test.rs |
| 4. Egress control | Assert web_fetch rejects POST, web_request blocks unknown domains | tests/egress_policy.rs |
| 5. Budget atomicity | Assert LLM call fails when budget exhausted | tests/agent/budget_test.rs |
| 6. Credential scanning | Assert credential messages blocked/redacted | tests/telegram/input_guard_test.rs |
| 7. Redactor chokepoint | Assert all tool paths pass through redactor | tests/redaction.rs |
| 8. Config split | Assert agent cannot write config.toml | tests/config/config_test.rs |

**Rules for flagging**:
- PR touches code related to an invariant but test NOT updated: flag as HIGH
- PR adds new public API in agent/ or executor/ without test: flag as HIGH
- PR modifies existing security test weakening assertion: flag as CRITICAL
- PR adds new public items without doc comments: flag as MEDIUM
- Any `#[cfg(test)]` module added under `src/`: flag as HIGH (test placement policy violation)

**OUTPUT FORMAT (JSON)**:
```json
{
  "invariants_touched": ["1", "3"],
  "regression_coverage": {
    "1": {"status": "COVERED", "note": "existing test still valid"},
    "3": {"status": "GAP", "note": "new config not tested"}
  },
  "summary": "Test coverage assessment",
  "comments": [
    {"path": "src/agent/policy.rs", "line": 100, "body": "**Issue**: [description]\n\n**Suggestion**: [fix]"}
  ]
}
```
"""

---

### 3. Sanitization (MANDATORY)

Before presenting to the user, you MUST sanitize ALL findings:

1. **Remove agent/AI markers**: Strip prefixes like "[Security]", "[Architecture]", etc.
2. **Remove AI references**: Remove "As an AI", "Generated by", any emoji markers, etc.
3. **Natural language**: Ensure comments read as if written by a human reviewer.
4. **Professional tone**: Use "Consider...", "This could be improved by...", "Potential issue:"
5. **Deduplicate**: Remove duplicate findings. Keep the version with more specific context.
6. **Filter low confidence**: Only include issues with confidence >= 60 (from Agent 3).
7. **EXCLUDE positive/informational comments**: Only include actionable issues.
8. **Format as Issue/Suggestion**: Every comment MUST follow:
   - `**Issue**: [description]`
   - `**Suggestion**: [fix]`
9. **Severity ordering**: CRITICAL > HIGH > MEDIUM > LOW.

### 4. Present Findings

1. **Aggregate**: Collect JSON outputs from all 4 agents.

2. **Merge & Sanitize**: Combine, deduplicate, prioritize.

3. **Present Security Gate Summary**:

   ## Security Gate

   | Invariant | Status | Note |
   |-----------|--------|------|
   | 1: No Host Executor | PRESERVED / AT RISK / ... | ... |
   | 2: Container Env Empty | ... | ... |
   | 3: No Container Network | ... | ... |
   | 4: Egress Controlled | ... | ... |
   | 5: Budget Atomic | ... | ... |
   | 6: Credential Scanning | ... | ... |
   | 7: Redactor Chokepoint | ... | ... |
   | 8: Config Split | ... | ... |

4. **Present ALL Findings** grouped by severity:

   ## Review Summary
   [Combined assessment]

   ---

   ## Findings ([N] total)

   ### 1. `src/executor/docker.rs:42` [CRITICAL]
   > **Issue**: [description]
   >
   > **Suggestion**: [recommendation]

   ### 2. `src/agent/loop.rs:15` [HIGH]
   > **Issue**: [description]
   >
   > **Suggestion**: [recommendation]

5. **Ask the user** which findings they want to fix now.

### 5. Review Evidence Block (MANDATORY)

Before returning final review output, include:

- The 4 agent IDs used for this run
- Total findings from each agent before dedupe
- Final deduped finding count
- Confirmation that both committed and uncommitted changes were considered

If any of the above is missing, the review is incomplete.
