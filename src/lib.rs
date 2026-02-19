//! Wintermute — a self-coding AI agent.
//!
//! Single Rust binary. Talks to you via Telegram. Writes tools to extend itself.
//! Privacy boundary: your data never leaves without your consent.
//!
//! See `DESIGN.md` for full architecture documentation.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod credentials;
pub mod executor;
pub mod logging;
pub mod memory;
pub mod providers;

// Future modules — uncomment as implemented:
// pub mod tools;
// pub mod agent;
// pub mod telegram;
// pub mod observer;
// pub mod heartbeat;
