//! Atomic budget tracking for token usage and tool call limits.
//!
//! Provides per-session and per-day budget enforcement using lock-free atomics.
//! The [`DailyBudget`] automatically resets when the calendar day changes.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use chrono::Utc;

use crate::config::BudgetConfig;

/// Errors produced when budget limits are exceeded.
#[derive(Debug, thiserror::Error)]
pub enum BudgetError {
    /// Session token limit exceeded.
    #[error("session token limit exceeded: used {used} of {limit}")]
    SessionLimitExceeded {
        /// Tokens already consumed this session.
        used: u64,
        /// Maximum tokens allowed per session.
        limit: u64,
    },

    /// Daily token limit exceeded.
    #[error("daily token limit exceeded: used {used} of {limit}")]
    DailyLimitExceeded {
        /// Tokens already consumed today.
        used: u64,
        /// Maximum tokens allowed per day.
        limit: u64,
    },

    /// Per-turn tool call limit exceeded.
    #[error("tool call limit exceeded: {count} of {limit}")]
    ToolCallsExceeded {
        /// Current tool call count.
        count: u32,
        /// Maximum tool calls per turn.
        limit: u32,
    },
}

/// Daily token budget shared across all sessions.
///
/// Automatically resets the counter when the ordinal day of the year changes.
#[derive(Debug)]
pub struct DailyBudget {
    tokens: AtomicU64,
    /// Ordinal day of the year for reset detection.
    ///
    /// Exposed for testing only — production code should not touch this.
    pub reset_day: AtomicU32,
    limit: u64,
}

impl DailyBudget {
    /// Create a new daily budget with the given token limit.
    pub fn new(limit: u64) -> Self {
        let today = current_ordinal_day();
        Self {
            tokens: AtomicU64::new(0),
            reset_day: AtomicU32::new(today),
            limit,
        }
    }

    /// Check whether `amount` additional tokens would exceed the daily limit.
    ///
    /// Resets the counter if the calendar day has changed since the last check.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetError::DailyLimitExceeded`] when the limit would be breached.
    pub fn check(&self, amount: u64) -> Result<(), BudgetError> {
        self.maybe_reset();
        let used = self.tokens.load(Ordering::Relaxed);
        let new_total = used.saturating_add(amount);
        if new_total > self.limit {
            return Err(BudgetError::DailyLimitExceeded {
                used,
                limit: self.limit,
            });
        }
        Ok(())
    }

    /// Record token usage (atomic add).
    pub fn record(&self, amount: u64) {
        self.tokens.fetch_add(amount, Ordering::Relaxed);
    }

    /// Current daily token usage.
    pub fn used(&self) -> u64 {
        self.maybe_reset();
        self.tokens.load(Ordering::Relaxed)
    }

    /// Configured daily token limit.
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Reset the counter if the calendar day has changed.
    fn maybe_reset(&self) {
        let today = current_ordinal_day();
        let stored = self.reset_day.load(Ordering::Relaxed);
        if stored != today {
            // Day changed — reset. Compare-exchange avoids double-reset races.
            if self
                .reset_day
                .compare_exchange(stored, today, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                self.tokens.store(0, Ordering::Relaxed);
            }
        }
    }
}

/// Per-session budget tracker.
///
/// Wraps a shared [`DailyBudget`] and adds session-scoped token tracking
/// plus tool-call-per-turn enforcement.
#[derive(Debug)]
pub struct SessionBudget {
    session_tokens: AtomicU64,
    daily: Arc<DailyBudget>,
    config: BudgetConfig,
}

impl SessionBudget {
    /// Create a new session budget backed by a shared daily budget.
    pub fn new(daily: Arc<DailyBudget>, config: BudgetConfig) -> Self {
        Self {
            session_tokens: AtomicU64::new(0),
            daily,
            config,
        }
    }

    /// Check whether `estimated_tokens` can be consumed without exceeding limits.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetError::SessionLimitExceeded`] or [`BudgetError::DailyLimitExceeded`].
    pub fn check_budget(&self, estimated_tokens: u64) -> Result<(), BudgetError> {
        let session_used = self.session_tokens.load(Ordering::Relaxed);
        let new_session = session_used.saturating_add(estimated_tokens);
        if new_session > self.config.max_tokens_per_session {
            return Err(BudgetError::SessionLimitExceeded {
                used: session_used,
                limit: self.config.max_tokens_per_session,
            });
        }
        self.daily.check(estimated_tokens)?;
        Ok(())
    }

    /// Record token usage from a completed LLM call.
    pub fn record_usage(&self, input_tokens: u64, output_tokens: u64) {
        let total = input_tokens.saturating_add(output_tokens);
        self.session_tokens.fetch_add(total, Ordering::Relaxed);
        self.daily.record(total);
    }

    /// Check whether the tool call count exceeds the per-turn limit.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetError::ToolCallsExceeded`] when the limit is breached.
    pub fn check_tool_calls(&self, count: u32) -> Result<(), BudgetError> {
        if count > self.config.max_tool_calls_per_turn {
            return Err(BudgetError::ToolCallsExceeded {
                count,
                limit: self.config.max_tool_calls_per_turn,
            });
        }
        Ok(())
    }

    /// Current session token usage.
    pub fn session_used(&self) -> u64 {
        self.session_tokens.load(Ordering::Relaxed)
    }

    /// Current daily token usage.
    pub fn daily_used(&self) -> u64 {
        self.daily.used()
    }
}

/// Returns the current ordinal day of the year (1-366).
fn current_ordinal_day() -> u32 {
    use chrono::Datelike;
    Utc::now().ordinal()
}
