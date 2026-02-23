//! Flatline — supervisor process for the Wintermute AI agent.
//!
//! Named after Dixie Flatline from Neuromancer. Monitors Wintermute via
//! filesystem (health.json, JSONL logs, git history), diagnoses failures
//! with rule-based patterns and LLM, applies fixes, and reports via Telegram.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Configuration loading and validation.
pub mod config;
/// SQLite state database for tool stats, fixes, and suppressions.
pub mod db;
/// LLM-based diagnosis for novel failures.
pub mod diagnosis;
/// Fix lifecycle: propose, apply, verify.
pub mod fixer;
/// Rule-based failure pattern matching.
pub mod patterns;
/// Telegram notification reporter.
pub mod reporter;
/// Rolling statistics engine for tool health and budget tracking.
pub mod stats;
/// Log tailing and health file monitoring.
pub mod watcher;
