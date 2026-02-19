# Wintermute

Self-coding AI agent: single Rust binary, Telegram interface, sandboxed Docker execution, persistent memory. Named after the AI in William Gibson's *Neuromancer*.

## Architecture

Wintermute receives user messages via Telegram, runs an LLM agent loop, and executes commands inside a hardened Docker container. The agent learns from interactions via a staged observer system and writes tools to extend itself over time.

See `DESIGN.md` for full architecture documentation.

**Components:**
- **Telegram Adapter** — input credential guard, HTML formatting, inline keyboards, file sending
- **Agent Loop** — per-session Tokio tasks with non-blocking dispatch
  - Context Assembler, Model Router, Tool Router, Policy Gate, Approval Manager, Egress Controller, Budget Tracker, Redactor
- **Executor** — DockerExecutor (production) or DirectExecutor (development), auto-detected
- **Tools** — 8 core tools + dynamic tools (agent-created, hot-reloaded from /scripts/)
- **Memory Engine** — SQLite with write-serialization actor, FTS5 search, optional vector (sqlite-vec)
- **Observer** — staged learning with configurable promotion (auto/suggest/off)
- **Heartbeat** — scheduled tasks, health checks, daily backup
- **Model Router** — default model, per-role and per-skill overrides (Anthropic + Ollama providers)

## Project Structure

```
src/
├── main.rs                    # CLI entry point (clap)
├── lib.rs                     # Library root
├── config.rs                  # Configuration loading and validation
├── providers/
│   ├── mod.rs                 # LlmProvider trait
│   ├── anthropic.rs           # Anthropic API + native tool calling
│   ├── ollama.rs              # Ollama API + native tool calling
│   └── router.rs              # ModelRouter (default → role → skill)
├── executor/
│   ├── mod.rs                 # Executor trait
│   ├── docker.rs              # DockerExecutor (bollard, warm container)
│   ├── direct.rs              # DirectExecutor (host, restricted dir)
│   └── redactor.rs            # Secret pattern redaction
├── tools/
│   ├── mod.rs                 # Tool routing (core + dynamic)
│   ├── core.rs                # 8 core tool implementations
│   ├── registry.rs            # Dynamic tool registry + hot-reload
│   ├── create_tool.rs         # create_tool implementation + git commit
│   └── browser.rs             # Browser bridge (Playwright subprocess)
├── agent/
│   ├── mod.rs                 # Session router (per-session tasks)
│   ├── loop.rs                # Agent loop (assemble → LLM → route → execute)
│   ├── context.rs             # Context assembly + trimming
│   ├── policy.rs              # Policy gate + egress rules
│   ├── approval.rs            # Non-blocking approval (short-ID callbacks)
│   └── budget.rs              # Token/cost budget (atomic counters)
├── memory/
│   ├── mod.rs                 # MemoryEngine
│   ├── writer.rs              # Write actor (mpsc)
│   ├── search.rs              # FTS5 + optional vector (sqlite-vec)
│   └── embedder.rs            # Embedder trait + OllamaEmbedder
├── telegram/
│   ├── mod.rs                 # Adapter (teloxide)
│   ├── input_guard.rs         # Credential detection + redaction
│   ├── ui.rs                  # HTML formatting, keyboards, file sending
│   └── commands.rs            # /status, /budget, /memory, /tools, etc.
├── observer/
│   ├── mod.rs                 # Observer pipeline
│   ├── extractor.rs           # LLM extraction (observer model)
│   └── staging.rs             # Pending → active promotion
└── heartbeat/
    ├── mod.rs                 # Tick loop
    ├── scheduler.rs           # Cron evaluation + task dispatch
    ├── backup.rs              # git bundle + sqlite backup
    └── health.rs              # Self-checks, log structured health
```

## Security Invariants

These MUST hold in every commit. Violation is a blocking review finding.

1. **No host executor** — `DockerExecutor` is the ONLY command execution path for user/LLM-generated commands. No `std::process::Command` or `tokio::process::Command` for user-controlled input on the host.
2. **Container env always empty** — No secrets injected into container environment.
3. **Container has no network** — Network mode is always `none`. All HTTP goes through host-side `web_fetch`/`web_request` tools.
4. **Egress controlled** — `web_fetch` is GET only (no body). `web_request` (POST/PUT/DELETE) is domain-allowlisted with approval for unknown domains. Browser follows same domain policy.
5. **Budget limits are atomic** — Counters checked before every LLM call. No unchecked paths.
6. **Inbound credential scanning** — User messages scanned for API key patterns before entering pipeline.
7. **Redactor is the single chokepoint** — ALL tool output passes through the redactor before entering LLM context. No bypass paths.
8. **Config split enforced** — Agent cannot modify `config.toml` (human-owned security policy). Only `agent.toml` is agent-writable.

## Code Rules

### Mandatory (violations are blocking)

1. `#![forbid(unsafe_code)]` in every crate root
2. `#![warn(missing_docs)]` in every crate root
3. No `unwrap()` — use `?`, `anyhow::Context`, or `ok_or_else`
4. `thiserror` for domain errors, `anyhow` for propagation
5. `tracing` macros for logging — never `println!` or `eprintln!`
6. Single-writer actor for all SQLite writes (mpsc channel to one Tokio task)
7. Container env must always be empty (`HashMap::new()`)
8. Redactor is the single chokepoint for ALL tool output
9. GNU `timeout` wraps every command executed in container
10. Derive order: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize`
11. HTML formatting for Telegram — never MarkdownV2
12. Do not place tests in `src/` (`#[cfg(test)]` modules are disallowed)

### Documentation

- Every public `struct`, `enum`, `trait`, and `fn` MUST have `///` doc comments
- Every module MUST have `//!` module-level documentation
- Non-obvious logic gets inline `//` comments explaining *why*, not *what*
- Examples in doc comments for complex public APIs

### Error Handling Pattern

```rust
/// Errors that can occur during sandbox operations.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// The requested container was not found.
    #[error("container not found: {0}")]
    NotFound(String),

    /// Command execution exceeded the configured timeout.
    #[error("execution timeout after {0}s")]
    Timeout(u64),

    /// Underlying Docker API error.
    #[error("docker error: {0}")]
    Docker(#[from] bollard::errors::Error),
}

// Propagation with context
use anyhow::{Context, Result};

fn do_thing() -> Result<()> {
    let config = load_config()
        .context("failed to load configuration")?;
    Ok(())
}
```

## Conventions

- **Async runtime**: Tokio (full features)
- **Serialization**: serde + serde_json for data, toml for config
- **Docker API**: bollard (pure Rust, no shelling out)
- **Telegram**: teloxide with inline keyboard support
- **HTTP client**: reqwest with manual redirect following (per-hop SSRF checks)
- **Database**: sqlx with migrations in `migrations/` directory
- **CLI**: clap with derive macros

## Tools (8 core)

| Tool | Behavior | Rate Limit | Approval |
|------|----------|------------|----------|
| `execute_command` | Shell in container. Timeout-wrapped. No network. | None | Policy gate |
| `create_tool` | Create/update dynamic tool in /scripts/ + git commit | None | No |
| `web_fetch` | GET only, no body, SSRF filtered | 30/min | No |
| `web_request` | POST/PUT/etc, domain allowlist | 10/min | New domains |
| `browser` | Playwright automation, domain policy. Host-side. | 60/min | New domains |
| `memory_search` | FTS5 + optional vector search | None | No |
| `memory_save` | Save a fact or procedure | None | No |
| `send_telegram` | Send message/file to user | None | No |

## Commit Convention

Use [Conventional Commits](https://www.conventionalcommits.org/). Every commit message must explain *why* the change was made.

**Format:**
```
type(scope): brief summary

[1-3 sentences explaining WHY this change was needed.]
```

**Types:** `feat`, `fix`, `refactor`, `test`, `docs`, `chore`, `ci`

**Scopes:** `providers`, `executor`, `tools`, `agent`, `memory`, `telegram`, `observer`, `heartbeat`, `config`

## Build & Run

```bash
cargo build --release
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
cargo deny check
cargo doc --no-deps
```

## Testing

- All tests must live under `tests/`; never add `#[cfg(test)]` modules in `src/`
- Mirror `src/` layout under `tests/` (for example, `src/providers/router.rs` -> `tests/providers/router_test.rs`)
- Keep top-level integration entrypoints aligned with top-level `src` modules (`tests/config.rs`, `tests/providers.rs`, etc.)
- Security invariants (1-8) must each have at least one dedicated test
- Use `#[tokio::test]` for async tests
- Mock Docker interactions with a test trait implementation

## Work Habits

- PRD files go in `tasks/` directory
- Update PRD progress as tasks complete
- Do not commit to git — suggest commit message at end of implementation
- In commit messages and PRs, do not mention AI tooling
- GitHub CLI alias is `github` (not `gh`)
