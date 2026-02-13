# PFAR Feature Spec: TOML Configuration

> **Feature**: Centralized TOML config file with env var overrides
> **Status**: Complete
> **Priority**: Infrastructure
> **Complexity**: Small — config structs, file loading, main.rs refactor

---

## Problem

All configuration was hardcoded as constants and env vars in `src/main.rs`. Adding new settings required editing Rust code. The spec (section 18.1) defines `./config.toml` as the standard config location.

---

## Solution

A `PfarConfig` struct in `src/config/mod.rs` that:

1. Loads from `./config.toml` (or `$PFAR_CONFIG_PATH`)
2. Falls back to defaults if no file exists (backward compatible)
3. Applies env var overrides on top (precedence: env > file > defaults)

```toml
# Example config.toml (in working directory)
[kernel]
log_level = "debug"
approval_timeout_seconds = 300

[paths]
audit_log = "~/.pfar/audit.jsonl"
journal_db = "~/.pfar/journal.db"

[llm.local]
base_url = "http://localhost:11434"
default_model = "llama3"

[llm.anthropic]
api_key = "vault:anthropic_api_key"
default_model = "claude-sonnet-4-20250514"

[adapter.telegram]
bot_token = "vault:telegram_bot_token"
owner_id = "415494855"
```

---

## Privacy Impact

None. Config loading is pre-pipeline infrastructure. No privacy invariants are affected.

---

## Env Var Overrides (backward compat)

| Env var | Config field |
|---------|-------------|
| `PFAR_CONFIG_PATH` | Config file path |
| `PFAR_AUDIT_LOG` | `paths.audit_log` |
| `PFAR_JOURNAL_PATH` | `paths.journal_db` |
| `PFAR_OLLAMA_URL` | `llm.local.base_url` |
| `PFAR_LOCAL_MODEL` | `llm.local.default_model` |
| `PFAR_ANTHROPIC_API_KEY` | `llm.anthropic.api_key` |
| `PFAR_ANTHROPIC_MODEL` | `llm.anthropic.default_model` |
| `PFAR_OPENAI_API_KEY` | `llm.openai.api_key` |
| `PFAR_OPENAI_MODEL` | `llm.openai.default_model` |
| `PFAR_LMSTUDIO_URL` | `llm.lmstudio.base_url` |
| `PFAR_LMSTUDIO_MODEL` | `llm.lmstudio.default_model` |
| `PFAR_TELEGRAM_BOT_TOKEN` | `adapter.telegram.bot_token` |
| `PFAR_TELEGRAM_OWNER_ID` | `adapter.telegram.owner_id` |
| `PFAR_SHUTDOWN_TIMEOUT_SECS` | `kernel.shutdown_timeout_seconds` |

---

## Implementation Checklist

- [x] Config structs with `#[derive(Deserialize)]` and `#[serde(default)]`
- [x] `PfarConfig::load()` with file loading + env overrides
- [x] Testable `apply_overrides(env_fn)` pattern (no unsafe `set_var`)
- [x] Refactor `main.rs` to remove all hardcoded constants
- [x] Refactor `build_inference_proxy(&LlmConfig)` and `resolve_owner_inference_config(&LlmConfig)`
- [x] 14 unit tests covering parsing, defaults, overrides, path resolution
