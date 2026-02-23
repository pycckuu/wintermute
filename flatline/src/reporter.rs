//! Telegram reporting for alerts, proposals, and daily health summaries.
//!
//! Uses teloxide Bot directly (send-only, no dispatcher). Messages are
//! prefixed with the configured prefix (default "Flatline").

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use teloxide::prelude::*;
use teloxide::types::ParseMode;
use tracing::{debug, warn};
use wintermute::heartbeat::health::HealthReport;

use crate::db::FixRecord;
use crate::patterns::PatternMatch;

/// Telegram reporter for Flatline notifications.
pub struct Reporter {
    bot: Bot,
    notify_users: Vec<i64>,
    prefix: String,
    /// Cooldown tracker to prevent duplicate alerts.
    cooldowns: HashMap<String, DateTime<Utc>>,
    cooldown_mins: u64,
}

impl Reporter {
    /// Create a new reporter.
    pub fn new(
        bot_token: &str,
        notify_users: Vec<i64>,
        prefix: String,
        cooldown_mins: u64,
    ) -> Self {
        Self {
            bot: Bot::new(bot_token),
            notify_users,
            prefix,
            cooldowns: HashMap::new(),
            cooldown_mins,
        }
    }

    /// Send an alert about a detected pattern.
    ///
    /// Respects cooldown: if this pattern key was alerted recently, the
    /// message is silently skipped.
    ///
    /// # Errors
    ///
    /// Returns an error if the Telegram API call fails.
    pub async fn send_alert(&mut self, pattern: &PatternMatch) -> anyhow::Result<()> {
        let key = format!("{:?}", pattern.kind);

        if self.is_in_cooldown(&key) {
            debug!(pattern = %key, "alert in cooldown, skipping");
            return Ok(());
        }

        let text = format!(
            "<b>{prefix} \u{2014} Alert</b>\n\n{summary}",
            prefix = html_escape(&self.prefix),
            summary = html_escape(&pattern.evidence.summary),
        );

        self.send_to_all(&text).await?;
        self.record_cooldown(&key);
        Ok(())
    }

    /// Send a fix proposal for user approval.
    ///
    /// # Errors
    ///
    /// Returns an error if the Telegram API call fails.
    pub async fn send_proposal(&mut self, fix: &FixRecord) -> anyhow::Result<()> {
        let diagnosis = fix.diagnosis.as_deref().unwrap_or("unknown issue");
        let action = fix.action.as_deref().unwrap_or("unknown action");

        let text = format!(
            "<b>{prefix} \u{2014} Proposal</b>\n\n\
             {diagnosis}\n\n\
             Proposed action: <code>{action}</code>",
            prefix = html_escape(&self.prefix),
            diagnosis = html_escape(diagnosis),
            action = html_escape(action),
        );

        self.send_to_all(&text).await
    }

    /// Send notification that a fix was applied.
    ///
    /// # Errors
    ///
    /// Returns an error if the Telegram API call fails.
    pub async fn send_fix_applied(&mut self, fix: &FixRecord) -> anyhow::Result<()> {
        let diagnosis = fix.diagnosis.as_deref().unwrap_or("unknown issue");
        let action = fix.action.as_deref().unwrap_or("unknown action");
        let verified = match fix.verified {
            Some(true) => "verified",
            Some(false) => "verification failed",
            None => "pending verification",
        };

        let text = format!(
            "<b>{prefix} \u{2014} Fix Applied</b>\n\n\
             {diagnosis}\n\n\
             Action: <code>{action}</code>\n\
             Status: {verified}",
            prefix = html_escape(&self.prefix),
            diagnosis = html_escape(diagnosis),
            action = html_escape(action),
            verified = html_escape(verified),
        );

        self.send_to_all(&text).await
    }

    /// Send daily health summary.
    ///
    /// # Errors
    ///
    /// Returns an error if the Telegram API call fails.
    pub async fn send_daily_health(
        &mut self,
        health: &HealthReport,
        tool_issues: &[(String, f64)],
    ) -> anyhow::Result<()> {
        let status_icon = if health.status == "running" {
            "\u{2705}"
        } else {
            "\u{26a0}\u{fe0f}"
        };
        let container_icon = if health.container_healthy {
            "\u{2705}"
        } else {
            "\u{274c}"
        };

        let limit = health.budget_today.limit;
        #[allow(clippy::cast_precision_loss)]
        let budget_pct = if limit > 0 {
            (health.budget_today.used as f64 / limit as f64) * 100.0
        } else {
            0.0
        };

        let uptime = format_uptime(health.uptime_secs);

        let mut text = format!(
            "<b>{prefix} \u{2014} Daily Health Report</b>\n\n\
             {status_icon} Wintermute: {status} (uptime {uptime})\n\
             {container_icon} Container: {container}\n\
             \u{2705} Budget: {budget_pct:.0}% used today",
            prefix = html_escape(&self.prefix),
            status = html_escape(&health.status),
            container = if health.container_healthy {
                "healthy"
            } else {
                "unhealthy"
            },
        );

        // Tool issues
        for (tool, rate) in tool_issues {
            text.push_str(&format!(
                "\n\u{26a0}\u{fe0f} {tool}: {:.0}% failure rate",
                rate * 100.0,
                tool = html_escape(tool),
            ));
        }

        text.push_str(&format!(
            "\n\u{2705} {} tools active",
            health.dynamic_tools_count
        ));

        self.send_to_all(&text).await
    }

    /// Check if an alert for this pattern is in cooldown.
    pub fn is_in_cooldown(&self, key: &str) -> bool {
        let Some(last_sent) = self.cooldowns.get(key) else {
            return false;
        };
        let elapsed = Utc::now().signed_duration_since(*last_sent);
        let mins_i64 = i64::try_from(self.cooldown_mins).unwrap_or(i64::MAX);
        let cooldown = chrono::Duration::minutes(mins_i64);
        elapsed < cooldown
    }

    /// Record a cooldown for a pattern key.
    pub fn record_cooldown(&mut self, key: &str) {
        self.cooldowns.insert(key.to_owned(), Utc::now());
    }

    /// Send a message to all configured notification users.
    async fn send_to_all(&self, text: &str) -> anyhow::Result<()> {
        if self.notify_users.is_empty() {
            return Ok(());
        }
        let mut any_sent = false;
        for &user_id in &self.notify_users {
            match self
                .bot
                .send_message(ChatId(user_id), text)
                .parse_mode(ParseMode::Html)
                .await
            {
                Ok(_) => any_sent = true,
                Err(e) => warn!(user_id, error = %e, "failed to send Telegram message"),
            }
        }
        if !any_sent {
            anyhow::bail!("failed to send Telegram message to any configured user");
        }
        Ok(())
    }
}

/// Format uptime seconds into a human-readable string (e.g. "3d 14h").
fn format_uptime(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;

    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

/// Escape HTML special characters for Telegram.
fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
