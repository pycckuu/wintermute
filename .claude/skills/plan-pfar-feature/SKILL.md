---
name: plan-pfar-feature
description: Plan a PFAR feature from description to actionable implementation plan. Gathers spec context, checks privacy invariants, analyzes from 4 lenses (architect, privacy, maintainability, performance), and outputs a concrete plan with file paths, type signatures, and PRD entries.
---

# Plan PFAR Feature: $ARGUMENTS

## Role
Act as a **Senior Rust Systems Architect** who deeply understands the PFAR v2 spec, its privacy invariants, and the codebase conventions.

## Rules
1. **Read before reasoning**: Always gather concrete project context before analyzing.
2. **Spec is authoritative**: Every design decision must trace to a spec section.
3. **Reuse over recreate**: Check `src/types/mod.rs` and existing modules before proposing new types.
4. **Privacy invariants are non-negotiable**: Every plan must explicitly state which invariants it touches and how it preserves them.
5. **Minimal implementation**: Match the spec exactly, do not over-engineer.

---

## Phase 0: Gather PFAR Context

Before analyzing, gather concrete information:

1. **Current progress**: Read `tasks/pfar-v2-prd.md` — identify completed vs. pending tasks
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
7. **Dependencies**: `cargo tree --depth 1` if new crates might be needed
8. **Recent commits**: `git log --oneline -10` for recent context

## Phase 1: Spec Alignment

Based on gathered context, answer:

1. **Spec sections**: Which sections (1-20) are directly relevant?
2. **Privacy invariants touched**: Which of the 11 invariants (A-K) does this feature interact with?
   - A: Session Isolation
   - B: Secrets Never Readable
   - C: Mandatory Label Enforcement
   - D: Graduated Taint-Gated Writes
   - E: Plan-Then-Execute Separation
   - F: Label-Based LLM Routing
   - G: Task Template Ceilings
   - H: No Tokens in URLs
   - I: Container GC
   - J: Capability = Designation + Permission + Provenance
   - K: Explicit Sink Routing
3. **PRD status**: Is this feature already in the PRD? Which task IDs?
4. **Pipeline impact**: Does this touch the 4-phase pipeline (Extract/Plan/Execute/Synthesize)?
5. **Trust boundary**: Does this change the trusted computing base boundary?

Present findings:
```
Spec Alignment:
  Sections: [list]
  Invariants: [list with impact notes]
  PRD tasks: [task IDs or "new — needs PRD entry"]
  Pipeline phases affected: [list or "none"]
  Trust boundary change: yes/no
```

## Phase 2: Multi-Perspective Analysis

### Architect Lens
- Which kernel module(s) should own this? (check module map from `src/kernel/CLAUDE.md`)
- New module or extension of existing module?
- What traits need implementing or extending? (check existing: `Tool`, `Adapter` patterns)
- How does data flow through the 4-phase pipeline for this feature?
- Are there generic bounds or lifetime considerations?

### Privacy Lens (PFAR-specific — replaces generic "Safety")
- Which invariants does this feature enforce, weaken, or not affect?
- Label implications: What `SecurityLabel` do inputs/outputs carry?
- Taint implications: Does this handle external data? What `TaintLevel`?
- Sink rules: Where can output go? What egress checks apply?
- Could this create a new prompt injection path? (Phase 1 must not see raw content)
- Does this create new tool access that needs template ceiling enforcement?

### Maintainability Lens
- Unit tests: `#[cfg(test)] mod tests` in each affected module
- Regression tests: Which of the 17 spec tests (section 17) are affected?
- Doc comments: `///` with spec section references (e.g., `/// Foo (spec 6.2).`)
- Clippy compliance: No unsafe, no unwrap, checked arithmetic
- Error handling: `thiserror` for domain errors, `anyhow` for propagation

### Performance Lens
- Pipeline latency impact: Does this add LLM calls? Container spawns?
- Allocation patterns: Avoid unnecessary `clone()`, `to_string()`
- Async considerations: Any blocking calls that need `spawn_blocking`?
- Hot paths: Is this called per-message or per-task?

## Phase 3: Solution Exploration

Generate 2-3 approaches:

| Approach | Core Idea | Pros | Cons | Complexity | Privacy Risk |
|----------|-----------|------|------|------------|--------------|

For each approach include:
- New types/traits introduced vs. reused from `src/types/mod.rs`
- New crates to add (check `deny.toml` compatibility)
- Files created or modified (with specific paths)
- Privacy invariant impact (preserved/strengthened/at-risk)
- Estimated test count

## Phase 4: Recommendation

Output a single actionable plan:

### 1. Chosen Approach
One-paragraph justification referencing spec sections.

### 2. Implementation Steps
Ordered list with specific file paths:
```
1. [file path] — what to do
2. [file path] — what to do
...
```

### 3. Key Type Signatures
New `struct`, `enum`, `trait`, `fn` signatures (Rust code blocks).
Mark which types already exist in `src/types/mod.rs` and should be reused.

### 4. Privacy Invariant Checklist
For each touched invariant:
- [ ] Invariant X: How it's preserved

### 5. Acceptance Criteria
- `cargo build` passes
- `cargo clippy --all-targets --all-features -- -D warnings` clean
- `cargo test --workspace` passes
- Specific behavioral tests

### 6. PRD Update
Task entries to add/update in `tasks/pfar-v2-prd.md`:
```
- [ ] X.Y Description of task
```

### 7. Feature Spec (if warranted)
For non-trivial features, suggest creating `docs/pfar-feature-<name>.md` following the format of existing feature specs (Problem, Solution, Privacy Impact, Implementation Checklist).

### 8. Risks & Mitigations
| Risk | Mitigation |
|------|------------|

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
- PRD: `tasks/pfar-v2-prd.md`
- Types: `src/types/mod.rs`
- Kernel: `src/kernel/` (module map in `src/kernel/CLAUDE.md`)
- Adapters: `src/adapters/`
- Tools: `src/tools/`
- Extractors: `src/extractors/`
- Config: `src/config/`
- Integration tests: `tests/`
