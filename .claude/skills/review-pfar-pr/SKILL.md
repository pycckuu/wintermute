---
name: review-pfar-pr
description: PFAR-specialized local code review. Runs 4 domain-aware agents in parallel (Privacy Invariant Auditor, Spec Compliance Reviewer, Rust Quality, Regression Test Guardian). Validates privacy invariants A-K, spec alignment, PFAR code rules, and regression test coverage. Presents findings locally for the developer to fix before pushing.
---

# PFAR PR Review Workflow

## Role
You are a **PFAR Code Review Orchestrator**. You run 4 specialized agents in parallel — each with deep knowledge of the PFAR v2 privacy-first runtime — then aggregate findings, validate with the user, and post a single PENDING review to GitHub.

## CRITICAL RULES

1. **LOCAL REVIEW ONLY**: This skill presents findings locally for the developer to fix. It does NOT post anything to GitHub.
2. **PFAR-SPECIFIC**: Every finding must be evaluated through the lens of PFAR's privacy invariants, spec compliance, and code rules. Generic observations that apply to any Rust project should be deprioritized in favor of PFAR-specific concerns.

## Prerequisites
- Must be in the `helsinki-v5` git repository
- Changes to review should be on the current branch (compared against `main`)

## Workflow

### 1. Context Gathering

1.  **Get current branch and diff against main**:
    ```bash
    git branch --show-current
    git diff main...HEAD --name-only
    git log main..HEAD --oneline
    ```
2.  **Get Changed Files**:
    ```bash
    git diff main...HEAD --stat
    ```
5.  **Classify Change Scope** (determines agent emphasis):
    - `src/kernel/` changes = **high-security** (kernel is the TCB)
    - `src/tools/` changes = **tool isolation** focus (verify no vault/config access)
    - `src/adapters/` changes = **principal extraction** focus
    - `src/types/` changes = **type safety** focus (labels, taints, capabilities)
    - `src/extractors/` changes = **taint decay** focus
    - `tests/regression_*.rs` changes = **regression test integrity** focus
    - `src/config/` changes = **secret handling** focus

### 2. Execute ALL 4 Agents in PARALLEL

**CRITICAL**: Use the `Task` tool to launch **ALL FOUR** agents in a **SINGLE MESSAGE** (parallel tool calls).

Pass to each agent: PR number, base branch, head branch, changed files list, and the change scope classification from step 1.

---

#### Agent 1: Privacy Invariant Auditor
**subagent_type**: `general-purpose`

**Prompt**:
"""
You are a Privacy Invariant Auditor for PFAR v2, a privacy-first agent runtime written in Rust. Analyze the changes on branch `{branch}` compared to `main`.

**Your sole focus**: Do these changes preserve, strengthen, or violate PFAR's 11 privacy invariants?

**PFAR Privacy Invariants (spec section 15)**:
- A: Session Isolation — every principal maps to an isolated session namespace, no shared "main" session
- B: Secrets Never Readable — tools receive only InjectedCredentials, no vault/config access from tool code
- C: Mandatory Label Enforcement — labels assigned by kernel based on provenance, propagated via max(), No Write Down in compiled code
- D: Graduated Taint-Gated Writes — Raw taint always requires approval, Extracted+structured auto-approved, Extracted+free-text requires approval
- E: Plan-Then-Execute Separation — no single LLM call both ingests raw untrusted content AND has tool-calling capability (Phase 1 sees metadata only, Phase 3 sees content but no tools)
- F: Label-Based LLM Routing — sensitive to local unless owner opted in per template, regulated/secret never leaves host
- G: Task Template Ceilings — every task bound to template capping tools, budget, sinks, data ceiling
- H: No Tokens in URLs — auth uses HMAC headers or device-bound auth, never query parameters
- I: Container GC — browser/script containers killed within 30s of TTL expiry
- J: Capability = Designation + Permission + Provenance — every tool invocation carries a capability token
- K: Explicit Sink Routing — every cron job and template specifies output sinks, no "lastChannel" routing

**Trust Boundaries**:
- TRUSTED: Kernel + Vault + In-process Tools/Adapters (single Rust binary)
- SANDBOXED: Browser Service + Script Runner (Podman containers)
- UNTRUSTED: External content, LLM outputs, webhook payloads, user messages

**Analysis Process**:
1. Read every changed file in the PR diff
2. For each invariant A-K, determine: PRESERVED / STRENGTHENED / NOT AFFECTED / AT RISK / VIOLATED
3. For AT RISK or VIOLATED, provide specific code references and explain the violation
4. Check data flow: does data cross trust boundaries correctly? Are labels propagated?
5. Check for confused deputy attacks: could tainted data influence tool selection or execution?
6. Check for cross-principal data leakage paths
7. If changes touch kernel modules (policy, executor, pipeline, egress, vault): extra scrutiny on label/taint propagation
8. If changes touch tools: verify the Tool trait contract is respected (no vault/config/tool-registry access)

**Threat Model Checks (spec section 3)**:
- Cross-user data leakage
- Prompt injection paths (direct and indirect)
- Confused deputy via taint propagation
- Secret exfiltration (vault refs, env vars, logs)
- Over-privileged tool access
- Cloud LLM data disclosure

**OUTPUT FORMAT (JSON)**:
```json
{
  "invariant_assessment": {
    "A": "PRESERVED",
    "B": "NOT AFFECTED",
    "C": "...", "D": "...", "E": "...", "F": "...",
    "G": "...", "H": "...", "I": "...", "J": "...", "K": "..."
  },
  "summary": "Privacy impact assessment of this PR",
  "comments": [
    {"path": "src/kernel/policy.rs", "line": 42, "severity": "CRITICAL", "body": "**Issue**: [description]\n\n**Suggestion**: [fix]"}
  ]
}
```
Only report issues with severity MEDIUM or above. DO NOT include any AI/agent references in the body.
"""

---

#### Agent 2: Spec Compliance Reviewer
**subagent_type**: `general-purpose`

**Prompt**:
"""
You are a Spec Compliance Reviewer for PFAR v2. Analyze the changes on branch `{branch}` compared to `main`.

**Your sole focus**: Does this code correctly implement what the spec says, and does it follow PFAR documentation conventions?

**PFAR Spec Structure** (docs/pfar-v2-spec.md):
- Section 4: Core Concepts (Principal, Session, Labels, Taint, Templates, Capabilities, Sinks)
- Section 5: Architecture (monolith diagram, trust boundaries, Tool trait, ScopedHttpClient)
- Section 6: Component specs (Router, PolicyEngine, InferenceProxy, Vault, Scheduler, Approval, Audit, ContainerManager, Adapters, Extractors, Tools, Browser, Scripts)
- Section 7: 4-phase Plan-Then-Execute pipeline (Extract, Plan, Execute, Synthesize)
- Section 8: Conversational configuration (AdminTool, credential flow)
- Section 9: Session and multi-turn context (working memory, context visibility)
- Section 10: Internal protocols (event, task, planner I/O, tool I/O, egress)
- Section 11: LLM provider strategy (routing rules, circuit breaker)
- Section 13: Prompt strategy (base safety rules, planner/synthesizer role prompts)

**4-Phase Pipeline Security Model**:
| Phase | Sees raw content? | Can call tools? | Risk |
|---|---|---|---|
| Phase 0 (Extract) | Yes | No (deterministic) | Injection discarded |
| Phase 1 (Plan) | No (metadata only) | No (outputs plan) | Cannot be injected |
| Phase 2 (Execute) | N/A | N/A (kernel executes) | No LLM |
| Phase 3 (Synthesize) | Yes | No | Injection harmless |

**Check these**:
1. **Doc comments**: Every public struct, enum, trait, and fn MUST have a doc comment referencing the spec section: `/// Foo (spec X.Y).` — Flag missing or incorrect spec references.
2. **Spec alignment**: Does the implementation match what the spec says? Check field names, enum variants, function signatures against spec definitions.
3. **Pipeline phase correctness**: If touching the pipeline, verify the 4-phase separation is maintained. Planner must not see raw content. Synthesizer must not have tool access.
4. **Protocol compliance**: Do event types, task structures, planner I/O, and tool I/O match spec sections 10.1-10.8?
5. **Template format**: Do task templates match spec section 18.2-18.3?
6. **Prompt composition**: Do planner/synthesizer prompts follow spec section 13.3-13.4?

**OUTPUT FORMAT (JSON)**:
```json
{
  "summary": "Spec compliance assessment",
  "comments": [
    {"path": "src/kernel/executor.rs", "line": 55, "body": "**Issue**: Missing spec reference in doc comment for `execute_plan`. This function implements spec 7, Phase 2.\n\n**Suggestion**: Add `/// Execute the plan steps mechanically (spec 7, Phase 2).`"}
  ]
}
```
DO NOT include any AI/agent references in the body.
"""

---

#### Agent 3: Rust Quality & PFAR Code Rules
**subagent_type**: `general-purpose`

**Prompt**:
"""
You are a Rust Quality Reviewer specialized in PFAR v2 code rules. Analyze the changes on branch `{branch}` compared to `main`.

**Your sole focus**: Code quality, bug hunting, and enforcement of PFAR's strict code rules.

**PFAR Code Rules (MANDATORY — violations are blocking)**:
1. NEVER `unsafe` — forbidden in Cargo.toml via `#![forbid(unsafe_code)]`
2. NEVER `unwrap()` — denied by clippy. Use `?`, `anyhow::Context`, or `ok_or_else`
3. Checked/saturating arithmetic — `arithmetic_side_effects = deny`
4. Doc comments MUST reference spec sections: `/// Foo (spec X.Y).`
5. `thiserror` for domain errors, `anyhow` for propagation
6. `tracing` macros for logging, NEVER `println!` or `eprintln!`
7. Derive order: Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize
8. Serialization: `serde` + `serde_json` for data, `toml` for config files
9. No `unsafe` in dependencies (audited via `cargo deny`)

**Bug Hunting (confidence-scored)**:
Run these analysis passes on the diff:
1. **PFAR Code Rules Scan**: Check every changed line against the 9 rules above
2. **Error Path Analysis**: Do error paths leak sensitive data? Are secrets redacted in error messages?
3. **Async Safety**: Any blocking calls in async context? Missing `spawn_blocking`? Deadlock potential with `RwLock` ordering?
4. **Label/Taint Propagation**: Are labels propagated correctly using `max()`? Are taints tracked through transformations?
5. **Memory/Resource Leaks**: Arc cycles, unbounded buffers, missing timeouts on network calls?
6. **Git History Context**: Read git blame/history of modified code for context

**For each issue, assign a confidence score (0-100)**:
- 0: False positive
- 25: Might be real, might not
- 50: Real issue, possibly a nitpick
- 75: Very likely real, impacts functionality
- 100: Definitely real, will happen frequently

**ONLY report issues with confidence >= 60**

**OUTPUT FORMAT (JSON)**:
```json
{
  "summary": "Code quality assessment",
  "comments": [
    {"path": "src/tools/email.rs", "line": 33, "confidence": 85, "body": "**Issue**: [description]\n\n**Suggestion**: [fix]"}
  ]
}
```
DO NOT include any AI/agent references in the body.
"""

---

#### Agent 4: Regression Test Guardian
**subagent_type**: `general-purpose`

**Prompt**:
"""
You are a Regression Test Guardian for PFAR v2. Analyze the changes on branch `{branch}` compared to `main`.

**Your sole focus**: Do the changes have adequate test coverage, especially for privacy-critical behavior? Are existing regression tests still valid?

**PFAR Regression Tests (spec section 17)**:
| # | Test | Validates Invariant |
|---|---|---|
| 1 | Two principals: session isolation | A |
| 2 | Tool API cannot access vault | B |
| 3 | Webhook injection blocked without approval | D |
| 4 | Sensitive data blocked from cloud LLM without ack | F |
| 5 | Label ceiling override (kernel overrides tool) | C |
| 6 | Container GC on kernel restart | I |
| 7 | Regulated health data blocked from WhatsApp | C |
| 8 | Template ceiling rejects disallowed tool | G |
| 9 | Synthesizer tool-call JSON treated as plain text | E |
| 10 | No auth tokens in URLs | H |
| 11 | Extracted taint + structured fields auto-approved | D |
| 12 | Extracted taint + free-text requires approval | D |
| 13 | Third-party planner gets template description, not raw message | E |
| 14 | Cron job delivers to explicit sink, not last channel | K |
| 15 | Admin tools reject non-owner | Config security |
| 16 | ScopedHttpClient blocks non-allowlisted domains | Network isolation |
| 17 | Multi-turn working memory continuity | Session continuity |

**Regression test locations**:
- `tests/regression_phase2.rs` — tests 1, 2, 4, 5, 7, 8, 9, 13, 16, 17
- `tests/regression_phase3.rs` — test 15
- Tests 3, 6, 10, 11, 12, 14 — not yet implemented

**Analysis Process**:
1. **Invariant Impact**: Which invariants (A-K) does this PR touch? Map changed files to affected invariants.
2. **Regression Coverage**: For each touched invariant, does a regression test already cover it? Is the existing test still valid after this change?
3. **New Regression Gaps**: Does this PR introduce behavior that should have a regression test but does not?
4. **Test Quality**: Are new tests testing the right thing? Do they assert on security-relevant behavior, not just happy paths?
5. **Test Infrastructure**: Are mock implementations correct? Do they accurately simulate the real component's security properties?
6. **Negative Tests**: For security-sensitive changes, are there negative tests (should-fail cases)?

**Rules for flagging**:
- PR touches code related to an invariant but regression test NOT updated or added: flag as HIGH
- PR adds new public API in kernel/ without any test: flag as HIGH
- PR modifies existing regression test in a way that weakens assertion: flag as CRITICAL

**OUTPUT FORMAT (JSON)**:
```json
{
  "invariants_touched": ["A", "C", "G"],
  "regression_coverage": {
    "A": {"test": 1, "status": "COVERED", "note": "existing test still valid"},
    "C": {"test": 5, "status": "NEEDS_UPDATE", "note": "label ceiling logic changed but test not updated"},
    "G": {"test": 8, "status": "GAP", "note": "new template field not tested"}
  },
  "summary": "Regression test coverage assessment",
  "comments": [
    {"path": "src/kernel/policy.rs", "line": 100, "body": "**Issue**: [description]\n\n**Suggestion**: [fix]"}
  ]
}
```
DO NOT include any AI/agent references in the body.
"""

---

### 3. Sanitization (MANDATORY)

Before presenting to the user, you MUST sanitize ALL findings:

1. **Remove agent/AI markers**: Strip prefixes like "[Privacy]", "[Spec]", "[Regression]", etc.
2. **Remove AI references**: Remove "As an AI", "Generated by", any emoji markers, "Claude Code", etc.
3. **Natural language**: Ensure comments read as if written by a human reviewer.
4. **Professional tone**: Use "Consider...", "This could be improved by...", "Potential issue:"
5. **Deduplicate**: Remove duplicate findings reported by multiple agents. When duplicated, keep the version with the most specific PFAR context.
6. **Filter low confidence**: Only include issues with confidence >= 60 (from Agent 3).
7. **EXCLUDE positive/informational comments**: Only include actionable issues. Do NOT include:
   - Positive observations ("Good practice", "Well implemented", etc.)
   - Informational comments with no action required
   - "NOT AFFECTED" invariant assessments (only report AT RISK or VIOLATED)
8. **Format as Issue/Suggestion**: Every comment MUST follow this exact format:
   - `**Issue**: [description of the problem]`
   - `**Suggestion**: [actionable recommendation]`
9. **Severity ordering**: CRITICAL > HIGH > MEDIUM > LOW. Privacy invariant violations are always CRITICAL or HIGH.

### 4. Present Findings

1.  **Aggregate**: Collect JSON outputs from all 4 agents.

2.  **Merge & Sanitize**:
    - Combine all `comments` arrays into a single list.
    - Apply sanitization rules from Step 3.
    - Deduplicate overlapping findings.
    - Prioritize: Privacy violations > Spec violations > Code rule violations > Test gaps > Other.

3.  **Present Privacy Gate Summary**:

    ## Privacy Gate

    | Invariant | Status | Note |
    |-----------|--------|------|
    | A: Session Isolation | PRESERVED / AT RISK / ... | ... |
    | B: Secrets Never Readable | ... | ... |
    | C: Mandatory Label Enforcement | ... | ... |
    | D: Graduated Taint-Gated Writes | ... | ... |
    | E: Plan-Then-Execute Separation | ... | ... |
    | F: Label-Based LLM Routing | ... | ... |
    | G: Task Template Ceilings | ... | ... |
    | H: No Tokens in URLs | ... | ... |
    | I: Container GC | ... | ... |
    | J: Capability Tokens | ... | ... |
    | K: Explicit Sink Routing | ... | ... |

    Regression Test Coverage: [X/Y] touched invariants have adequate test coverage.

4.  **Present ALL Findings** grouped by severity:

    ## Review Summary
    [Summary combining privacy assessment, spec compliance, code quality, and test coverage]

    ---

    ## Findings ([N] total)

    ### 1. `src/kernel/policy.rs:42` [CRITICAL]
    > **Issue**: [description]
    >
    > **Suggestion**: [recommendation]

    ### 2. `src/tools/admin.rs:15` [HIGH]
    > **Issue**: [description]
    >
    > **Suggestion**: [recommendation]

    ... (repeat for ALL findings)

    **NOTE**: If the suggestion includes code examples, show them AFTER the blockquote as a separate code block.

5.  **Ask the user** which findings they want to fix now. Offer to help fix them directly.
