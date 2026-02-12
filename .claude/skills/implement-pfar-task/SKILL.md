---
name: implement-pfar-task
description: Orchestrate the PFAR development lifecycle for a task — from spec reading to PR creation.
---

# Implement Task: $ARGUMENTS

## Role
Act as a **Senior Tech Lead & Orchestrator**. Guide the implementation through a rigorous, spec-driven process with security review built in.

## Rules
1. **Sequential Execution**: Follow phases in order. Do not skip quality checks.
2. **Spec First**: Always read the relevant spec section before writing any code.
3. **Human Gates**: STOP and ask for confirmation before **committing** and **creating the PR**.
4. **Progress Tracking**: Update `tasks/pfar-v2-prd.md` checkboxes after each phase.

---

### Phase 1: Preparation
1. **Read PRD**: Open `tasks/pfar-v2-prd.md`, find the task (e.g. `1.4`).
2. **Read Spec**: Open `docs/pfar-v2-spec.md` and read the section(s) referenced by the task.
3. **Review Types**: Read `src/types/mod.rs` — identify reusable types. Do NOT recreate existing types.
4. **Scan Related Code**: Check existing modules for patterns to follow.

### Phase 1.5: Branch Verification
1. Check current branch: `git branch --show-current`
2. Branch MUST match: `pycckuu/<kebab-case-title>`
   - Example: `pycckuu/policy-engine-taint-checking`
3. If wrong, rename: `git branch -m pycckuu/<kebab-case-title>`
4. Show user the branch name and confirm.

### Phase 1.75: Design
1. **Outline the approach**: Based on spec reading, identify:
   - New structs, enums, traits needed (or existing ones to reuse from `src/types/mod.rs`)
   - Error types to define (`thiserror`)
   - Files to create or modify
   - Key functions/methods and their signatures
   - How this module interacts with other kernel components
   - Privacy invariants this module must uphold
2. **Present the design** to the user:
   ```
   Design for Task X.Y:
   ----------------------------------------
   [Outline: types, files, key functions, interactions, invariants]
   ----------------------------------------
   Proceed with implementation? (Yes/No/Adjust)
   ```
3. **Wait for approval** before writing any code.

### Phase 2: Implementation
1. **Implement** the task following the approved design and CLAUDE.md code rules:
   - No unsafe, no unwrap, doc comments with spec refs
   - `thiserror` for errors, `tracing` for logging
   - For kernel modules: follow `/implement-kernel-module` workflow
2. **Verify**: `cargo build && cargo test`
3. **Update PRD**: Mark implementation step in progress.

### Phase 3: Quality Assurance
Execute in order:

1. **Refactor** — `/code-refactorer`
   - Improve structure and readability. Verify logic is intact.

2. **Code Review** — `/code-reviewer`
   - Address ALL CRITICAL and HIGH findings.

3. **Security Review** — use `security-reviewer` agent
   - Review against PFAR's 10 privacy invariants (spec section 4).
   - Address any privacy violations before proceeding.

4. **Lint** — `/code-linter`
   - Run `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings`
   - Fix warnings in changed files only.

### Phase 4: Documentation
1. **Claude Config**: Run `/update-claude-config` to check if any Claude Code files need updates (CLAUDE.md, kernel CLAUDE.md, skills, agents, MEMORY.md).
2. **README**: Does it reference changed functionality? Update if so.
3. **PRD**: Mark QA as complete in `tasks/pfar-v2-prd.md`.

### Phase 5: Delivery
1. **Draft Commit**:
   ```
   type(scope): brief summary of change

   [1-3 sentences explaining WHY this change was needed.
   What problem does it solve? Do NOT use co-authored-by.]
   ```
   Example:
   ```
   feat(kernel/policy): implement label propagation with No Read Up / No Write Down

   Enforces mandatory access control at the kernel level so that data
   at a given security label cannot flow to lower-trust sinks. This is
   the core privacy guarantee of PFAR's information-flow model.
   ```

2. **STOP**: Present draft to user.
   ```
   Proposed Commit Message:
   ----------------------------------------
   [Draft Message]
   ----------------------------------------
   Ready to commit? (Yes/No)
   ```

3. **Execute**:
   - Commit with confirmed message
   - Push: `git push -u origin <branch>`
   - Create PR: `github pr create --title "..." --body "..."`
   - PR Review: `/pr-reviewer`
   - Mark task complete in PRD: `- [x]`

---

## Master Checklist

- [ ] **Task & Spec Read** (PRD task + spec section)
- [ ] **Branch Verified** (`pycckuu/<kebab-case>`)
- [ ] **Design Approved** (types, files, functions, invariants)
- [ ] **Implementation Complete** & Tests Pass
- [ ] **Refactored** (`/code-refactorer`)
- [ ] **Code Reviewed** (`/code-reviewer`) — CRITICAL/HIGH fixed
- [ ] **Security Reviewed** (`security-reviewer` agent) — privacy invariants OK
- [ ] **Linted** (`/code-linter`) — clean
- [ ] **Claude Config Updated** (`/update-claude-config`)
- [ ] **Commit Verified & Applied**
- [ ] **PR Created & Reviewed** (`/pr-reviewer`)
- [ ] **PRD Updated** (`tasks/pfar-v2-prd.md`)
