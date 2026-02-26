# Playwright Browser Bridge

## Introduction/Overview

The browser tool (`src/tools/browser.rs`) has validation, rate-limiting, and SSRF checks built, but no concrete `BrowserBridge` implementation. This task adds a Docker sidecar running Playwright + Flask and an HTTP bridge so the agent can automate a browser.

## Goals

- Provide a working `BrowserBridge` implementation via HTTP to a Playwright Docker sidecar
- Follow the existing sidecar pattern from `executor/egress.rs`
- Respect all security invariants (no `process::Command` in `src/`)

## Tasks

- [x] 1.0 Make `ensure_network` and `NETWORK_NAME` pub(crate) in `src/executor/egress.rs`
- [x] 2.0 Create `src/executor/playwright.rs` — sidecar lifecycle manager
- [x] 3.0 Add `pub mod playwright;` to `src/executor/mod.rs`
- [x] 4.0 Create `src/tools/browser_bridge.rs` — HTTP BrowserBridge implementation
- [x] 5.0 Add `pub mod browser_bridge;` to `src/tools/mod.rs`
- [x] 6.0 Add `BrowserConfig` to `src/config.rs`
- [x] 7.0 Add `[browser]` section to `config.example.toml`
- [x] 8.0 Wire up browser bridge in `src/main.rs`
- [x] 9.0 Create `tests/executor/playwright_test.rs` and register in `tests/executor.rs`
- [x] 10.0 Create `tests/tools/browser_bridge_test.rs` and register in `tests/tools.rs`
- [x] 11.0 Fix test helpers in `tests/agent/loop_test.rs` and `tests/agent/session_test.rs` to include `browser` field

## Verification

- [x] `cargo build --all-targets` passes
- [x] `cargo clippy --all-targets --all-features -- -D warnings` passes clean
- [x] `cargo fmt --check` passes
- [x] `cargo doc --no-deps` passes
- [x] No `std::process::Command` or `tokio::process::Command` in `src/`
