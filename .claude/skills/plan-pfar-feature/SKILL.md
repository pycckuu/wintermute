---
name: plan-pfar-feature
description: Plan a PFAR feature with adversarial review cycles. Gathers context, designs minimal approach, runs mandatory Skeptic Review with complexity scorecard, validates privacy invariants as pass/fail gate, and outputs a concrete plan.
---

# Plan PFAR Feature: $ARGUMENTS

## Role
Act as a **Senior Rust Systems Architect** who deeply understands the PFAR v2 spec, its privacy invariants, and the codebase conventions.

## Rules
1. **Read before reasoning**: Always gather concrete project context before analyzing.
2. **Spec is authoritative**: Every design decision must trace to a spec section.
3. **Reuse over recreate**: Check `src/types/mod.rs` and existing modules before proposing new types.
4. **Privacy invariants are non-negotiable**: Every invariant touched must pass the Privacy Gate.
5. **Simplest first**: Design the minimal approach before considering alternatives. The burden of proof is on complexity, not simplicity.
6. **Justify every addition**: Every new file, type, and trait requires a written reason why existing code can't do it.
7. **Skeptic Review is mandatory**: Phase 3 cannot be skipped or abbreviated. The complexity scorecard must be filled in completely.

---

## Phase 0: Gather PFAR Context

Before analyzing, gather concrete information:

1. **Current progress**: Read `docs/pfar-v2-prd.md` — identify completed vs. pending tasks
2. **Module map**: Read `src/kernel/CLAUDE.md` — check which modules exist and their status
3. **Reusable types**: Read `src/types/mod.rs` — these are fully implemented, do NOT recreate:
   - `Principal`, `PrincipalClass`, `SecurityLabel`, `TaintLevel`, `TaintSet`
   - `CapabilityToken`, `Task`, `TaskState`, `InboundEvent`, `LabeledEvent`
   - `ApprovalDecision`, `ToolResult`
4. **Related code**: Search for keywords from the feature description:
   - `rg "relevant_keyword" --type rust src/`
   - `rg "^pub (fn|struct|enum|trait)" --type rust src/kernel/`
5. **Spec sections**: Read relevant sections from `docs/pfar-v2-spec.md`:
   - Section numbers map to: 1=Overview, 2=Goals, 3=Threats, 4=Core Concepts, 5=Architecture, 6=Components, 7=Pipeline, 8=Config, 9=Sessions, 10=Protocols, 11=LLM, 12=Integrations, 13=Prompts, 14=Operations, 15=Invariants, 16=Security, 17=Regression Tests, 18=Config Ref, 19=Impl Plan, 20=Limitations
6. **Existing feature specs**: Check `docs/pfar-feature-*.md` for related features
7. **Comparable feature**: Find the most similar existing feature spec and note its complexity (LoC, files changed). This is your baseline.
8. **Dependencies**: `cargo tree --depth 1` if new crates might be needed
9. **Recent commits**: `git log --oneline -10` for recent context

## Phase 1: Spec Alignment

Based on gathered context, produce an **Alignment Card**:

```
Feature:            [name]
Spec sections:      [list]
Invariants:         [A-K with impact: enforces / preserves / not-affected]
  A: Session Isolation           — [impact]
  B: Secrets Never Readable      — [impact]
  C: Mandatory Label Enforcement — [impact]
  D: Graduated Taint-Gated Writes — [impact]
  E: Plan-Then-Execute Separation — [impact]
  F: Label-Based LLM Routing     — [impact]
  G: Task Template Ceilings      — [impact]
  H: No Tokens in URLs           — [impact]
  I: Container GC                — [impact]
  J: Capability Tokens           — [impact]
  K: Explicit Sink Routing       — [impact]
PRD tasks:          [existing IDs or "new — needs PRD entry"]
Pipeline impact:    [phases affected or "none"]
Trust boundary:     [changed or "unchanged"]
Comparable feature: [name] — [X files, ~Y LoC]
```

## Phase 2: Design

Analyze through two lenses, then produce ONE minimal design proposal.

### Architecture Lens
- Which kernel module(s) should own this? (check module map from `src/kernel/CLAUDE.md`)
- New module or extension of existing module?
- What traits need implementing or extending?
- How does data flow through the 4-phase pipeline for this feature?
- Latency impact: Does this add LLM calls? Container spawns? Hot path considerations?
- Async considerations: Any blocking calls that need `spawn_blocking`?

### Minimal Viable Approach Lens
- What existing code already does 80% of this?
- Could this be a change to existing modules instead of new ones?
- What is the smallest public API surface that works?
- Can this be done WITHOUT adding any new types?
- What would a 10-line version of this look like?

### Design Proposal

Output ONE approach (not 2-3):

```
Design Proposal:
  New files:      [list — with one-line justification for EACH]
  New types:      [list — with one-line justification for EACH]
  Modified files: [list]
  Estimated LoC:  [number]
  Key signatures: [Rust code block with fn/struct/enum signatures]
```

Mark which types already exist in `src/types/mod.rs` and should be reused.

---

## Phase 3: Skeptic Review (MANDATORY)

**Do not skip this phase.** Answer all 7 questions in writing.

### 1. "Do Nothing" Test
What breaks if we don't build this? If the answer is "nothing breaks, it would just be nice" — shrink scope or defer.

### 2. "10-Line Patch" Test
Could a tiny change to existing code achieve 80% of the value? Describe what that patch would look like. If it works, do that instead.

### 3. New Files Justification
For EACH new file in the proposal: Why can't this code go in an existing file? "Cleaner as separate" is not sufficient — the file must have a genuinely different responsibility.

### 4. New Types Justification
For EACH new struct, enum, or trait: Does an equivalent exist in `src/types/mod.rs` or the target module? Could this be a type alias, a field on an existing type, or a method instead?

### 5. Deletion Pass
Go through every element in the proposal (file, type, function, test). Mark each:
- **ESSENTIAL**: Required to satisfy the spec
- **NICE-TO-HAVE**: Improves quality but not spec-required

Remove everything marked NICE-TO-HAVE from the proposal.

### 6. Complexity Scorecard

```
Complexity Scorecard:
  New files:         [count] (target: 0-1)
  New types/traits:  [count] (target: 0-3)
  Modified files:    [count] (target: 1-3)
  Estimated new LoC: [count] (target: < 300)
  New dependencies:  [count] (target: 0)
```

If ANY number exceeds its target, write one sentence justifying why.

### 7. Senior Engineer Test
"Would a senior engineer look at this PR and say 'this is too much for what it does'?" Write one honest sentence.

### Revised Design

After answering all 7 questions, produce the **revised** design proposal. Note what was cut and why.

---

## Phase 4: Privacy Gate (pass/fail)

For each invariant marked as "enforces" or "preserves" in the Alignment Card:

```
Invariant [X]: [name]
  Status:    PRESERVED / STRENGTHENED / AT RISK
  Mechanism: [one sentence explaining how the implementation preserves this]
```

**Gate rule**: If any invariant is AT RISK and no fix is identified, the plan CANNOT proceed. Revise the design until all invariants pass.

---

## Phase 5: Final Plan

### 1. Summary
One paragraph: what this feature does and why, referencing spec sections.

### 2. Implementation Steps
Ordered list with specific file paths:
```
1. [file path] — what to do
2. [file path] — what to do
...
```

### 3. Key Code Sketches
New `struct`, `enum`, `trait`, `fn` signatures (Rust code blocks).
Mark which types already exist in `src/types/mod.rs` and should be reused.

### 4. Privacy Invariant Checklist
From Phase 4 gate results:
- [ ] Invariant X: How it's preserved

### 5. Acceptance Criteria
- `cargo build` passes
- `cargo clippy --all-targets --all-features -- -D warnings` clean
- `cargo test --workspace` passes
- Specific behavioral tests

### 6. PRD Update
Task entries to add/update in `docs/pfar-v2-prd.md`:
```
- [ ] X.Y Description of task
```

### 7. Feature Spec (if warranted)
For non-trivial features, suggest creating `docs/pfar-feature-<name>.md` following the format of existing feature specs (Problem, Solution, Privacy Impact, Implementation Checklist).

---

## PFAR Codebase Quick Reference

**Code conventions** (from CLAUDE.md):
- NEVER `unsafe` (forbidden in Cargo.toml)
- NEVER `unwrap()` — use `?`, `anyhow::Context`, or `ok_or_else`
- Checked/saturating arithmetic
- Doc comments reference spec: `/// Foo (spec X.Y).`
- `thiserror` for domain errors, `anyhow` for propagation
- `tracing` macros, NEVER `println!`
- Derive order: Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize
- Serde + serde_json for data, toml for config

**Key paths**:
- Spec: `docs/pfar-v2-spec.md`
- PRD: `docs/pfar-v2-prd.md`
- Types: `src/types/mod.rs`
- Kernel: `src/kernel/` (module map in `src/kernel/CLAUDE.md`)
- Adapters: `src/adapters/`
- Tools: `src/tools/`
- Extractors: `src/extractors/`
- Config: `src/config/`
- Integration tests: `tests/`
