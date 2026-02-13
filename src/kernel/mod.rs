//! Kernel core — the trusted computing base (spec sections 5, 6, 7).
//!
//! All security enforcement happens here: label assignment, taint
//! propagation, capability tokens, sink access control.

pub mod approval;
pub mod audit;
pub mod egress;
pub mod executor;
pub mod inference;
pub mod journal;
pub mod pipeline;
pub mod planner;
pub mod policy;
pub mod router;
pub mod session;
pub mod synthesizer;
pub mod template;
pub mod vault;

// Sub-modules to be added as implementation progresses:
// - scheduler: Cron Scheduler (spec 6.5)
// - container: Container Manager (spec 6.8)
