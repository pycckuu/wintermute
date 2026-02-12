---
name: quality-check
description: Run full quality gate for PFAR project
disable-model-invocation: true
---

# Quality Check

Run the full quality gate and report results:

1. Format check: `cargo fmt --all -- --check`
2. Lint check: `cargo clippy --all-targets --all-features -- -D warnings`
3. Test suite: `cargo test --workspace --verbose`

Report pass/fail for each step. If any step fails, show the errors and suggest fixes.
