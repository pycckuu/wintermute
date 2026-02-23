//! Rolling statistics engine for tool health and budget tracking.
//!
//! Aggregates `LogEvent` data into hourly buckets stored in the state database,
//! and provides query methods for failure rates and budget burn analysis.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use chrono::Timelike;
use wintermute::heartbeat::health::HealthReport;

use crate::db::StateDb;
use crate::watcher::LogEvent;

/// Aggregates tool execution events and queries derived statistics.
pub struct StatsEngine {
    db: Arc<StateDb>,
}

impl StatsEngine {
    /// Create a new stats engine backed by the given state database.
    pub fn new(db: Arc<StateDb>) -> Self {
        Self { db }
    }

    /// Ingest a batch of log events, aggregating tool_call events into hourly buckets.
    ///
    /// Only events with `event == "tool_call"` and a non-empty `tool` name are processed.
    /// Each event is bucketed by the hour of its timestamp, then recorded in the database.
    ///
    /// # Errors
    ///
    /// Returns an error if any database write fails.
    pub async fn ingest(&self, events: &[LogEvent]) -> anyhow::Result<()> {
        // Group events by (tool_name, hourly_bucket).
        let mut buckets: HashMap<(String, String), Vec<&LogEvent>> = HashMap::new();

        for event in events {
            if event.event.as_deref() != Some("tool_call") {
                continue;
            }

            let tool_name = match event.tool.as_deref() {
                Some(name) if !name.is_empty() => name,
                _ => continue,
            };

            let bucket = match event.ts.as_deref() {
                Some(ts) => truncate_to_hour(ts),
                None => continue,
            };

            buckets
                .entry((tool_name.to_owned(), bucket))
                .or_default()
                .push(event);
        }

        for ((tool_name, window_start), bucket_events) in &buckets {
            for event in bucket_events {
                let success = event.success.unwrap_or(false);
                let duration_ms = event.duration_ms.and_then(|d| i64::try_from(d).ok());

                self.db
                    .record_tool_stat(tool_name, window_start, success, duration_ms)
                    .await
                    .with_context(|| {
                        format!("failed to record stat for tool={tool_name} bucket={window_start}")
                    })?;
            }
        }

        Ok(())
    }

    /// Calculate the failure rate for a tool over a rolling window.
    ///
    /// Returns a value between 0.0 (no failures) and 1.0 (all failures).
    /// Returns 0.0 if there are no recorded events in the window.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn tool_failure_rate(&self, tool: &str, window_hours: u64) -> anyhow::Result<f64> {
        let since = hours_ago(window_hours);
        let rows = self.db.tool_stats(tool, &since).await?;

        let mut total_success: i64 = 0;
        let mut total_failure: i64 = 0;

        for row in &rows {
            total_success = total_success.saturating_add(row.success_count);
            total_failure = total_failure.saturating_add(row.failure_count);
        }

        let total = total_success.saturating_add(total_failure);
        if total == 0 {
            return Ok(0.0);
        }

        #[allow(clippy::cast_precision_loss)]
        let rate = total_failure as f64 / total as f64;
        Ok(rate)
    }

    /// List tools with failure rates above the given threshold.
    ///
    /// Returns a list of (tool_name, failure_rate) pairs sorted by rate descending.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn failing_tools(
        &self,
        threshold: f64,
        window_hours: u64,
    ) -> anyhow::Result<Vec<(String, f64)>> {
        let since = hours_ago(window_hours);
        let tool_names = self.db.distinct_tool_names(&since).await?;

        let mut failing = Vec::new();

        for tool_name in tool_names {
            let rate = self.tool_failure_rate(&tool_name, window_hours).await?;
            if rate > threshold {
                failing.push((tool_name, rate));
            }
        }

        // Sort by failure rate descending (NaN treated as equal).
        failing.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(failing)
    }

    /// Calculate the budget burn rate as a ratio.
    ///
    /// Compares the fraction of daily budget already used against the fraction
    /// of the day elapsed (UTC). A value > 1.0 means the budget is being
    /// consumed faster than uniform daily pace.
    ///
    /// Returns 0.0 if the budget limit is zero.
    pub async fn budget_burn_rate(&self, health: &HealthReport) -> f64 {
        let used = health.budget_today.used;
        let limit = health.budget_today.limit;

        if limit == 0 {
            return 0.0;
        }

        #[allow(clippy::cast_precision_loss)]
        let budget_fraction = used as f64 / limit as f64;

        let day_fraction = day_fraction_elapsed();
        if day_fraction <= 0.0 {
            return budget_fraction;
        }

        budget_fraction / day_fraction
    }
}

/// Fraction of the current UTC day that has elapsed (0.0 to 1.0).
///
/// Returns 0.0 at the very start of the day (midnight UTC).
pub fn day_fraction_elapsed() -> f64 {
    let now = chrono::Utc::now();
    let seconds_into_day = i64::from(now.hour())
        .saturating_mul(3600)
        .saturating_add(i64::from(now.minute()).saturating_mul(60))
        .saturating_add(i64::from(now.second()));

    const SECONDS_PER_DAY: i64 = 86400;

    if seconds_into_day <= 0 {
        return 0.0;
    }

    #[allow(clippy::cast_precision_loss)]
    let fraction = seconds_into_day as f64 / SECONDS_PER_DAY as f64;
    fraction
}

/// Truncate an ISO 8601 timestamp to its hour start.
///
/// Example: "2026-02-19T14:30:00Z" becomes "2026-02-19T14:00:00+00:00".
fn truncate_to_hour(ts: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
        // Each `with_*` returns `None` only for out-of-range values, which
        // cannot happen when zeroing minutes/seconds/nanoseconds. Still,
        // fall back to the original timestamp instead of panicking.
        let truncated = dt
            .with_minute(0)
            .and_then(|d| d.with_second(0))
            .and_then(|d| d.with_nanosecond(0))
            .unwrap_or(dt);
        truncated.to_rfc3339()
    } else if ts.len() >= 13 {
        // Fallback: take "YYYY-MM-DDTHH" and append ":00:00Z".
        format!("{}:00:00Z", &ts[..13])
    } else {
        ts.to_owned()
    }
}

/// Calculate an RFC 3339 timestamp for `hours` hours ago.
fn hours_ago(hours: u64) -> String {
    let now = chrono::Utc::now();
    let hours_i64 = i64::try_from(hours).unwrap_or(i64::MAX);
    let duration = chrono::Duration::hours(hours_i64);
    let since = now.checked_sub_signed(duration).unwrap_or(now);
    since.to_rfc3339()
}
