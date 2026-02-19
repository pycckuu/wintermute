---
name: plan-wintermute-task
description: Plan a Wintermute implementation task using multi-perspective analysis. Gathers project context, reads DESIGN.md, analyzes from 4 lenses (architect, security, maintainability, performance), explores solutions, and outputs a concrete implementation plan.
---

# Plan Wintermute Task Workflow

## PART 1: ANALYSIS

### Phase 0: Context Gathering

Before analyzing, gather concrete information:

1. **Project structure**: `tree -L 3 -I 'target'`
2. **Crate layout**: `cat Cargo.toml`
3. **Design doc** (MUST READ): `cat DESIGN.md`
4. **Map to implementation phase**: Identify which DESIGN.md phase/task this work relates to (Phase 1: Foundation, Phase 2: Core Loop, Phase 3: Intelligence).
5. **Recent related commits**: `git log --oneline -20`
6. **Search for similar patterns**: `rg "relevant_keyword" --type rust`
7. **Check existing traits/types**: `rg "^pub (struct|enum|trait|type)" --type rust`
8. **Review dependencies**: `cargo tree --depth 1`

### Phase 1: Repository Comprehension

Based on gathered context:

- Map crate/module boundaries (`lib.rs`, `mod.rs` structure)
- Identify public API surface (`pub` exports)
- **Identify affected Wintermute component**: Which of these is touched?
  - `providers/` — LlmProvider trait, Anthropic, Ollama, ModelRouter
  - `executor/` — Executor trait, DockerExecutor, DirectExecutor, Redactor
  - `tools/` — Tool routing, 8 core tools, dynamic registry, create_tool, browser
  - `agent/` — Session router, agent loop, context assembly, policy gate, approval, budget
  - `memory/` — Write actor, FTS5 search, optional vector, embedder
  - `telegram/` — Adapter, input guard, UI formatting, commands
  - `observer/` — Session tracker, extractor, staging
  - `heartbeat/` — Scheduler, backup, health
  - `config.rs` — Configuration loading
- **Map data flow** through the affected component
- **Check trust boundaries**: Does this change cross a trust boundary?
  - HOST (trusted): Rust binary, all components
  - CONTAINER (sandboxed): Docker container, /workspace, /scripts
  - EXTERNAL (untrusted): LLM outputs, web content, user messages
- Document conventions:
  - Error handling: `thiserror` for domain, `anyhow` for propagation
  - Async runtime: Tokio with full features
  - Serialization: serde derives, toml for config
  - Logging: `tracing` macros only
- Note `#[cfg(...)]` feature flags in use

### Phase 2: Multi-Perspective Analysis

#### Architect Lens
- Which module(s) should own this functionality?
- What traits need implementing or extending?
- Does this need new tools or modify the existing 8-tool schema?
- Does this affect the agent loop flow?
- Does this affect the Model Router (default → role → skill)?
- Are there generic bounds or lifetime considerations?

#### Wintermute Security Lens (CRITICAL)

For each security invariant, answer explicitly:

1. **No host executor** — Does this introduce any path for host command execution? Any `std::process::Command`?
2. **Container env empty** — Does this require injecting anything into container env?
3. **No container network** — Does this need container network access?
4. **Egress controlled** — Does this add new egress paths? New HTTP endpoints? Browser actions on new domains?
5. **Budget atomic** — Does this bypass budget checks? Add LLM calls without budget gates?
6. **Credential scanning** — Does this handle user input without credential scanning?
7. **Redactor chokepoint** — Does this produce tool output that bypasses the redactor?
8. **Config split** — Does this allow the agent to modify config.toml?

For each: answer YES (needs mitigation) or NO (safe). If YES, describe the mitigation.

#### Maintainability Lens
- Unit tests (`#[cfg(test)]` modules) — what needs testing?
- Integration tests (`tests/` directory) — any new invariant tests needed?
- Documentation: `///` doc comments on all new public items, `//!` module docs
- Does it follow the actor pattern for SQLite writes?
- Does it use bollard (not shell-out) for Docker?
- Clippy compliance

#### Performance Lens
- Does it block the Tokio runtime? (blocking I/O, sync mutex)
- Does it affect the per-session concurrency model?
- Does it impact budget tracking latency?
- Allocation patterns (avoid unnecessary `clone()`, `to_string()`)
- Async considerations (blocking calls → `spawn_blocking`)

### Phase 3: Solution Exploration

Generate 2-3 approaches with:

| Approach | Core Idea | Pros | Cons | Complexity | Risks |
|----------|-----------|------|------|------------|-------|

Include for each:
- New types/traits introduced
- Crates to add (if any)
- Files touched
- Security invariants affected

### Phase 4: Recommendation

Output a single actionable plan:

1. **Chosen Approach**: One-paragraph justification
2. **Implementation Steps**: Ordered, with specific file paths
3. **Type Signatures**: Key new `struct`, `enum`, `trait`, `fn` signatures (with doc comments)
4. **Security Checklist**: Which invariants were reviewed, mitigations applied
5. **Acceptance Criteria**: `cargo test`, `cargo clippy`, specific behaviors, invariant tests
6. **Risks & Mitigations**
