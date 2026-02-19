---
name: implement-wintermute-task
description: Orchestrate the full Wintermute development lifecycle for a task from ticket creation to implementation, quality assurance, and pull request. Includes security invariant verification and documentation checks.
---

# Implement Wintermute Task Workflow

## Role

Act as a **Senior Tech Lead & Orchestrator** for the Wintermute project. Guide the user through a rigorous development process that enforces Wintermute's security invariants, documentation standards, and Conventional Commits at every stage.

## Rules

1. **Sequential Execution**: Follow phases in order. Do not skip quality checks.
2. **Specialized Tools**:
   - **Agents**: Specialized AI personas for execution (`/coder`, `/code-refactorer`, `/review-wintermute-pr`, `/code-linter`, `/pr-reviewer`)
3. **Human Gates**: Stop and ask for confirmation before:
   - Committing changes (must show draft message)
   - Creating the PR
4. **Security First**: Every phase must consider Wintermute's 8 security invariants.
5. **Documentation Required**: All new public items must have doc comments.
6. **Test Placement Policy**: All tests must live in `tests/` and mirror the `src/` structure. Never add `#[cfg(test)]` modules in `src/`.

## Non-Skippable Execution Contract

These steps are **mandatory** for every `/implement-wintermute-task` run:

1. Run implementation.
2. Run refactor (`/code-refactorer`).
3. Run security/code review (`/review-wintermute-pr`).
4. Run linter pass (`/code-linter`).
5. Run final verification commands.

You MUST NOT mark work as complete until all five are executed and evidenced.

- If a step fails, fix and re-run that step.
- If a step cannot run due to environment limitations, explicitly state the blocker and ask the user whether to proceed.
- A step may be skipped **only** when the user explicitly asks to skip it.
- Never claim “done” with a missing mandatory step.

## Workflow Steps

### Phase 1: Preparation & Tracking

1. **Read Architecture Context**:
   ```bash
   cat DESIGN.md
   ```
   Identify which implementation phase/task this work relates to.


### Phase 1.5: Branch Name Verification

1. **Check Current Branch**:
   ```bash
   git branch --show-current
   ```

2. **Verify Pattern**: Branch name MUST follow:
   ```
   pycckuu/<kebab-case-title>
   ```

3. **If Pattern is Incorrect**, rename:
   ```bash
   git branch -m pycckuu/<kebab-case-title>
   ```

### Phase 2: Implementation

1. **Plan First** (if complex):
   - **Skill**: `/plan-wintermute-task`
   - Ensures security invariants are considered before writing code.

2. **Execute Logic**: Implement the core functionality.
   - **Agent**: `/coder`
   - **Verification**: After implementation:
     ```bash
     cargo build --all-targets && cargo test --all-features
     ```

3. **Security Invariant Quick Check** (MANDATORY):
   ```bash
   # Invariant 1: No host executor
   rg "std::process::Command|tokio::process::Command" src/ --type rust

   # Invariant 2: Container env empty (should find only HashMap::new())
   rg "container_env" src/ --type rust

   # Invariant 7: Redactor chokepoint (all tool output paths)
   rg "redact" src/ --type rust
   ```
   If any violations found, fix before proceeding.

### Phase 3: Refinement & Quality Assurance

Execute in order (**all mandatory**):

1. **Refactor**: Improve structure and readability.
   - **Agent**: `/code-refactorer`
   - Verify: logic remains intact, doc comments preserved.

2. **Security Review**: Deep Wintermute-aware analysis.
   - **Skill**: `/review-wintermute-pr`
   - This runs 4 parallel agents: Security Invariant Auditor, Architecture Compliance, Rust Quality & Code Rules, Regression & Test Coverage Guardian.
   - **Action**: Address ALL CRITICAL and HIGH findings.
   - If findings are fixed, run review again to confirm no unresolved CRITICAL/HIGH issues remain.

3. **Lint**: Final style and safety check.
   - **Agent**: `/code-linter`
   - Fix any remaining linter warnings in changed files only.

4. **Test Layout Verification**:
   ```bash
   rg "#\\[cfg\\(test\\)\\]" src/ --type rust
   ```
   This must produce no matches. Ensure any new tests are placed under mirrored `tests/<module>/` paths.

5. **Doc Coverage Check**:
   ```bash
   cargo doc --no-deps 2>&1 | grep "warning"
   ```
   Ensure all new public items have `///` doc comments.

6. **Final Build/Test Verification**:
   ```bash
   cargo build --all-targets
   cargo test --all-features
   cargo clippy --all-targets --all-features -- -D warnings
   ```
   Do not proceed to delivery until these pass.

### Phase 4: Documentation

1. **Scan for Impact**:
   ```bash
   fd -e md -e rst -e txt --exclude target
   ```

2. **Update Project Docs**:
   - **DESIGN.md**: Does it reference changed functionality?
   - **PRD in tasks/**: Update task progress in the relevant PRD file.

3. **Update Agent Instructions** (if conventions changed):
   - **`dev/AGENT.md`**: Update if any of these changed:
     - New module added or module renamed → update Project Structure
     - New security invariant or rule modified → update Security Invariants
     - New tool added or tool signature changed → update Core Tools table
     - New code rule or convention established → update Code Rules
     - New commit scope introduced → update Commit Convention
     - New build/test command needed → update Build & Run Commands
   - **`dev/skills/*.md`**: Update if the workflow itself needs adjustment:
     - New security grep pattern needed → update `implement-wintermute-task.md` Phase 2
     - New quality check required → update `quality-check-wintermute.md`
     - New review concern area → update `review-wintermute-pr.md`

4. **Verify Doc Comments**:
   - All new public `struct`, `enum`, `trait`, `fn` have `///` doc comments
   - All new modules have `//!` module-level docs
   - Non-obvious logic has inline `//` comments explaining *why*

### Phase 5: Delivery

1. **Draft Commit**: Prepare a **Conventional Commit** message.

   **Format:**
   ```
   type(scope): brief summary

   [1-3 sentences explaining WHY this change was needed.
   What problem does it solve? What was the motivation?]
   ```

   **Types:** `feat`, `fix`, `refactor`, `test`, `docs`, `chore`, `ci`

   **Scopes:** `providers`, `executor`, `tools`, `agent`, `memory`, `telegram`, `observer`, `heartbeat`, `config`

   **Example:**
   ```
   feat(executor): add timeout wrapping for container commands

   Without timeouts, a hung process inside the container could block the
   agent loop indefinitely. GNU timeout ensures reliable process kill,
   with a Tokio backstop as secondary protection.
   ```

2. **Verify**: **STOP** and present the draft to the user.
   ```text
   Proposed Commit Message:
   ----------------------------------------
   [Draft Message Here]
   ----------------------------------------
   Ready to commit and ship? (Yes/No)
   ```

3. **Execution** (after user approval):
   - **Commit**: Apply the confirmed message.
   - **Create PR**: `/create-pr`
   - **PR Analysis**: `/pr-reviewer`

### Phase 6: Evidence Report (MANDATORY)

Before final completion message, present an evidence block with:

- Commands executed and pass/fail status for:
  - `cargo build --all-targets`
  - `cargo test --all-features`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - Security quick-check grep commands
- Agent/skill runs and identifiers:
  - `/code-refactorer`
  - `/review-wintermute-pr`
  - `/code-linter`
- Any unresolved findings (must be empty or explicitly approved by user)

If this evidence block is incomplete, the task is not complete.

## Master Checklist

- [ ] **Architecture Context Read** (DESIGN.md)
- [ ] **Branch Named Correctly** (`pycckuu/<kebab-case-title>`)
- [ ] **Implementation Complete** (`/coder`) & Tests Pass
- [ ] **Security Invariants Verified** (quick grep scan — no violations)
- [ ] **Refactored** (`/code-refactorer`) — executed and evidenced
- [ ] **Security Reviewed** (`/review-wintermute-pr`) — executed and evidenced; CRITICAL/HIGH fixed
- [ ] **Linted** (`/code-linter`) — executed and evidenced; clean
- [ ] **Test Layout Verified** — no `#[cfg(test)]` in `src/`; tests mirrored under `tests/`
- [ ] **Doc Comments Complete** — All new public items documented
- [ ] **Documentation Updated** (DESIGN.md/PRD)
- [ ] **Agent Instructions Updated** (dev/AGENT.md, dev/skills/ — if conventions changed)
- [ ] **Final Build/Test/Clippy Passed** (all three commands)
- [ ] **Evidence Report Included** (commands + agent runs + remaining findings)
- [ ] **Conventional Commit Verified & Applied**
- [ ] **PR Created**
- [ ] **PR Analyzed** (`/pr-reviewer`)
