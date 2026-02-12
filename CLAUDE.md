# PFAR v2

Privacy-First Agent Runtime v2. Single Rust binary with mandatory access control.

## Key References

- Spec (authoritative): @docs/pfar-v2-spec.md
- Task tracking: @tasks/pfar-v2-prd.md — update checkboxes as tasks complete
- Contributing: @CONTRIBUTING.md
- Core types in `src/types/mod.rs` are fully implemented — do not recreate

## Build & Test

```sh
cargo build
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace --verbose
```

Full pre-push (includes cargo-deny and typos): `.githooks/pre-push`

## Code Rules

- NEVER use `unsafe` (forbidden in Cargo.toml)
- NEVER use `unwrap()` (denied by clippy) — use `?`, `anyhow::Context`, or `ok_or_else`
- Use checked/saturating arithmetic (arithmetic_side_effects = deny)
- Doc comments MUST reference spec sections: `/// Foo (spec X.Y).`
- Use `thiserror` for domain errors, `anyhow` for propagation
- Use `tracing` macros for logging, NEVER `println!`
- Derive order: Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize
- Serialization: `serde` + `serde_json` for data, `toml` for config files

## Workflow

- IMPORTANT: Always read the relevant spec section before implementing a component
- Do NOT commit to git — suggest commit message at end of implementation
- Do NOT add co-authored-by lines in commits or PRs
- GitHub CLI alias: `github` (not `gh`)
- Update `tasks/pfar-v2-prd.md` progress after completing tasks
- Keep implementations minimal — match the spec exactly, do not over-engineer

## Testing

- Unit tests: `#[cfg(test)] mod tests` inline in each module
- Integration tests: `tests/` directory at crate root
- Use `#[tokio::test]` for async tests
- 17 regression tests defined in spec section 17
