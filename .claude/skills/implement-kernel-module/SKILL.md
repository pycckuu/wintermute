---
name: implement-kernel-module
description: Implement a PFAR kernel sub-module following spec-driven workflow
---

# Implement Kernel Module: $ARGUMENTS

Follow this workflow to implement a kernel sub-module:

1. **Read the spec**: Open `docs/pfar-v2-spec.md` and read the section(s) relevant to this module
2. **Check PRD**: Read `docs/pfar-v2-prd.md` to find the specific task(s) for this module
3. **Review existing types**: Read `src/types/mod.rs` — reuse existing types, do not recreate
4. **Check kernel mod.rs**: Read `src/kernel/mod.rs` for the module structure
5. **Implement**: Create the module file(s) under `src/kernel/`
   - Add `pub mod <name>;` to `src/kernel/mod.rs`
   - Follow code rules: no unsafe, no unwrap, doc comments with spec refs
   - Use `thiserror` for module-specific errors
   - Use `tracing` for logging
6. **Write tests**: Add `#[cfg(test)] mod tests` with unit tests covering key invariants
7. **Quality check**: Run `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --workspace`
8. **Update PRD**: Mark completed task(s) in `docs/pfar-v2-prd.md`
9. **Suggest commit**: Provide a commit message (no co-authored-by)
