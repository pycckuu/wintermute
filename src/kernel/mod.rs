//! Kernel core — the trusted computing base (spec sections 5, 6, 7).
//!
//! All security enforcement happens here: label assignment, taint
//! propagation, capability tokens, sink access control.

pub mod audit;
pub mod inference;
pub mod policy;
pub mod router;
pub mod template;
pub mod vault;

// Sub-modules to be added as implementation progresses:
// - scheduler: Cron Scheduler (spec 6.5)
// - approval:  Approval Queue (spec 6.6)
// - container: Container Manager (spec 6.8)
// - pipeline:  Plan-Then-Execute Pipeline (spec 7)
