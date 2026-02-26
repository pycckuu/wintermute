//! Atomic budget tracking for token usage and tool call limits.
//!
//! Provides per-session and per-day budget enforcement using lock-free atomics.
//! The [`DailyBudget`] automatically resets when the calendar day changes.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use chrono::Utc;

use crate::config::BudgetConfig;

/// Warning thresholds as percentage of session budget.
const WARNING_THRESHOLDS: [u8; 3] = [70, 85, 95];

/// Current budget consumption status with graduated warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetStatus {
    /// Usage is below all warning thresholds.
    Ok,
    /// Usage has crossed a warning threshold.
    Warning {
        /// Human-readable warning level description.
        level: &'static str,
        /// Current usage percentage (0–100).
        percent: u8,
    },
    /// Budget is fully exhausted.
    Exhausted,
}

/// Which budget scope triggered the status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetScope {
    /// Session token budget.
    Session,
    /// Daily token budget.
    Daily,
}

impl BudgetScope {
    /// Human-readable label for display.
    pub fn label(self) -> &'static str {
        match self {
            Self::Session => "Session",
            Self::Daily => "Daily",
        }
    }

    /// Compute remaining tokens for this scope from a [`SessionBudget`].
    pub fn remaining(self, budget: &SessionBudget) -> u64 {
        match self {
            Self::Session => budget.session_limit().saturating_sub(budget.session_used()),
            Self::Daily => budget.daily_limit().saturating_sub(budget.daily_used()),
        }
    }
}

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

impl BudgetError {
    /// Which scope this error belongs to.
    pub fn scope(&self) -> BudgetScope {
        match self {
            Self::SessionLimitExceeded { .. } | Self::ToolCallsExceeded { .. } => {
                BudgetScope::Session
            }
            Self::DailyLimitExceeded { .. } => BudgetScope::Daily,
        }
    }
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
/// plus tool-call-per-turn enforcement. When the session budget is exhausted,
/// the session pauses and resumes with a fresh allocation on the next user
/// message (see [`SessionBudget::renew`]).
#[derive(Debug)]
pub struct SessionBudget {
    session_tokens: AtomicU64,
    daily: Arc<DailyBudget>,
    config: BudgetConfig,
    /// Whether the session is paused due to budget exhaustion.
    paused: AtomicBool,
}

impl SessionBudget {
    /// Create a new session budget backed by a shared daily budget.
    pub fn new(daily: Arc<DailyBudget>, config: BudgetConfig) -> Self {
        Self {
            session_tokens: AtomicU64::new(0),
            daily,
            config,
            paused: AtomicBool::new(false),
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

    /// Maximum tokens allowed for this session.
    pub fn session_limit(&self) -> u64 {
        self.config.max_tokens_per_session
    }

    /// Maximum tokens allowed per day.
    pub fn daily_limit(&self) -> u64 {
        self.config.max_tokens_per_day
    }

    /// Session usage as a percentage (0–100), clamped.
    pub fn session_percent(&self) -> u8 {
        percent_of(self.session_used(), self.config.max_tokens_per_session)
    }

    /// Daily usage as a percentage (0–100), clamped.
    pub fn daily_percent(&self) -> u8 {
        percent_of(self.daily_used(), self.config.max_tokens_per_day)
    }

    /// Reset the session token counter for a new budget window.
    ///
    /// The daily budget is unaffected — it still caps total spend across all
    /// windows. Returns `false` if the daily budget is also exhausted,
    /// leaving the session paused. On success the caller should also call
    /// [`set_paused(false)`](Self::set_paused) to resume processing.
    ///
    /// Note: the daily-used check is a relaxed load, so another session could
    /// push daily usage over the limit between this check and the next
    /// `check_budget()`. This is benign — `check_budget()` independently
    /// validates the daily budget before every LLM call.
    pub fn renew(&self) -> bool {
        if self.daily.used() >= self.daily.limit() {
            return false;
        }
        self.session_tokens.store(0, Ordering::Relaxed);
        true
    }

    /// Whether the session is paused due to budget exhaustion.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Mark the session as paused (budget exhausted) or unpaused (renewed).
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// Compute the current budget status considering both session and daily limits.
    ///
    /// Returns the highest-severity status across both scopes. Exhausted is
    /// checked first; then warning thresholds are evaluated in descending order.
    pub fn budget_status(&self) -> (BudgetStatus, BudgetScope) {
        let session_pct = self.session_percent();
        let daily_pct = self.daily_percent();

        // Check exhaustion first
        if session_pct >= 100 {
            return (BudgetStatus::Exhausted, BudgetScope::Session);
        }
        if daily_pct >= 100 {
            return (BudgetStatus::Exhausted, BudgetScope::Daily);
        }

        // Determine highest warning level for each scope
        let session_warning = highest_warning(session_pct);
        let daily_warning = highest_warning(daily_pct);

        // Pick whichever scope crossed the highest warning threshold
        match session_warning.max(daily_warning) {
            Some(threshold) => {
                // Tie-break: prefer session scope when thresholds are equal
                let scope = if session_warning >= daily_warning {
                    BudgetScope::Session
                } else {
                    BudgetScope::Daily
                };
                (warning_status(threshold), scope)
            }
            None => (BudgetStatus::Ok, BudgetScope::Session),
        }
    }
}

/// Compute usage percentage, clamped to 0–100.
fn percent_of(used: u64, limit: u64) -> u8 {
    if limit == 0 {
        return 100;
    }
    #[allow(clippy::cast_possible_truncation, clippy::arithmetic_side_effects)]
    {
        (used.saturating_mul(100) / limit).min(100) as u8
    }
}

/// Return the highest warning threshold that `pct` has crossed, if any.
fn highest_warning(pct: u8) -> Option<u8> {
    WARNING_THRESHOLDS
        .iter()
        .rev()
        .find(|&&threshold| pct >= threshold)
        .copied()
}

/// Build a `BudgetStatus::Warning` from a threshold percentage.
fn warning_status(threshold: u8) -> BudgetStatus {
    let level = match threshold {
        95 => "critical",
        85 => "high",
        70 => "elevated",
        _ => "warning",
    };
    BudgetStatus::Warning {
        level,
        percent: threshold,
    }
}

/// Returns the current ordinal day of the year (1-366).
fn current_ordinal_day() -> u32 {
    use chrono::Datelike;
    Utc::now().ordinal()
}
