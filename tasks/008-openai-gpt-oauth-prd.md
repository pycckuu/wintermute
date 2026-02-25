# OpenAI GPT Provider + OAuth Auth — PRD

**Branch:** `openai-gpt-oauth`
**Status:** COMPLETED

---

## Summary

Add first-class `openai/<model>` support so Wintermute can run on GPT models instead of Anthropic when configured. Authentication must support OAuth token usage, with API key fallback.

Scope includes provider implementation, credential resolution, router wiring, redactor secret registration, config/docs updates, and tests.

Out of scope: removing Anthropic support.

## Gap Map

| Feature | Status | Gap |
|---------|--------|-----|
| `openai/<model>` in router | Missing | `src/providers/router.rs` only supports `anthropic` + `ollama` |
| OpenAI provider implementation | Missing | No `src/providers/openai.rs` module |
| OpenAI auth resolution | Missing | `src/credentials.rs` has Anthropic OAuth/API-key flow only |
| Startup secret registration | Partial | Startup only resolves external Anthropic auth for secret registration/logging |
| Config/bootstrap docs | Missing | Examples and init `.env` template only mention Anthropic credentials |
| Provider tests | Missing | No OpenAI wire-format, contract, or router credential tests |

## Auth Requirements

OpenAI credential resolution order:

1. `OPENAI_OAUTH_TOKEN` from loaded `.env` credentials
2. `OPENAI_API_KEY` from loaded `.env` credentials

Rules:

- Treat blank values as missing.
- OAuth token takes precedence over API key.
- All resolved auth secrets must be registered with Redactor.
- Never log raw token/key values.

## Implementation Tasks

- [x] 1. Add OpenAI auth type and resolver in `src/credentials.rs`
- [x] 2. Add `src/providers/openai.rs` implementing `LlmProvider` via OpenAI Chat Completions API with tool-calling support
- [x] 3. Export `openai` module in `src/providers/mod.rs`
- [x] 4. Wire `openai` branch in `src/providers/router.rs` with clear missing-credential errors
- [x] 5. Update startup wiring in `src/main.rs` to resolve/register OpenAI auth secrets and add provider-aware auth logging
- [x] 6. Update bootstrap `.env` template to include `OPENAI_API_KEY` and `OPENAI_OAUTH_TOKEN`
- [x] 7. Update `config.example.toml` and relevant docs with `openai/gpt-*` examples
- [x] 8. Add tests for credentials resolution priority and secret exposure behavior
- [x] 9. Add OpenAI provider wire-format tests (`tests/providers/openai_test.rs`)
- [x] 10. Extend provider contract/router tests for OpenAI model specs
- [x] 11. Run `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, and targeted/full test suites

## Files to Modify

| File | Change |
|------|--------|
| `src/credentials.rs` | Add `OpenAiAuth` enum + `resolve_openai_auth()` + `secret_values()` |
| `src/providers/openai.rs` | New provider implementation and request/response mapping |
| `src/providers/mod.rs` | `pub mod openai;` and module docs update |
| `src/providers/router.rs` | Add `openai` provider instantiation path |
| `src/main.rs` | Resolve OpenAI auth, include secrets in redactor registration, adjust auth logs |
| `config.example.toml` | Add GPT/OpenAI model examples (default or commented override) |
| `DESIGN.md` | Update provider list/examples |
| `tests/credentials/oauth_test.rs` | Add OpenAI OAuth/API key resolution tests |
| `tests/providers/openai_test.rs` | New OpenAI build/parse tests |
| `tests/providers/provider_contract_test.rs` | Add OpenAI capability + model-id checks |
| `tests/providers/router_test.rs` | Add router coverage for `openai/<model>` specs |

## Security Invariants

Must remain true after implementation:

1. No host command execution changes (`process::Command` remains off-limits).
2. Container env policy unchanged (no secret injection into container env).
3. Redactor remains a single chokepoint and receives OpenAI OAuth/API secrets.
4. Budget and policy gates unchanged for all model calls.

## Acceptance Criteria

- Config with `models.default = "openai/gpt-5"` starts successfully when OpenAI OAuth token is present.
- OAuth token is preferred over API key when both are set.
- If OAuth is missing, API key fallback works.
- If both are missing, router returns actionable missing-credential error for `openai`.
- OpenAI tool-call responses map to existing `ContentPart::ToolUse` format.
- No auth secret appears in logs/tool output/test snapshots.
- Full build, fmt, clippy, and tests pass.

## Notes for Operator

Minimal config/env switch for GPT:

```toml
[models]
default = "openai/gpt-5"
```

```bash
OPENAI_OAUTH_TOKEN=...
# or OPENAI_API_KEY=...
```
