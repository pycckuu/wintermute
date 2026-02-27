//! Monthly tool review: identify unused, failing, or slow tools.
//!
//! Called as a builtin scheduled task. Reads `_meta` from all dynamic tools
//! and produces a summary report sent to the user via Telegram.

use tokio::sync::mpsc;
use tracing::info;

use crate::agent::TelegramOutbound;
use crate::telegram::ui::escape_html;
use crate::tools::registry::DynamicToolRegistry;

/// Execute the monthly tool review and send results via Telegram.
///
/// Identifies:
/// - **Unused tools**: `last_used` is `None` or > 30 days ago.
/// - **Failing tools**: `success_rate < 0.70`.
/// - **Slow tools**: `avg_duration_ms > 10_000`.
///
/// # Errors
///
/// Returns an error if sending the Telegram message fails.
pub async fn execute_tool_review(
    registry: &DynamicToolRegistry,
    telegram_tx: &mpsc::Sender<TelegramOutbound>,
    user_id: i64,
) -> anyhow::Result<String> {
    let schemas = registry.all_schemas();

    let mut unused: Vec<String> = Vec::new();
    let mut failing: Vec<(String, f64)> = Vec::new();
    let mut slow: Vec<(String, u64)> = Vec::new();

    let now = chrono::Utc::now();
    let thirty_days = chrono::Duration::days(30);

    for schema in &schemas {
        let meta = match &schema.meta {
            Some(m) => m,
            None => {
                unused.push(schema.name.clone());
                continue;
            }
        };

        // Check unused: no last_used or > 30 days.
        let is_unused = match &meta.last_used {
            None => true,
            Some(ts) => {
                if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts) {
                    now.signed_duration_since(parsed) > thirty_days
                } else {
                    true
                }
            }
        };
        if is_unused {
            unused.push(schema.name.clone());
        }

        // Check failing: success_rate < 0.70.
        if meta.invocations > 0 && meta.success_rate < 0.70 {
            failing.push((schema.name.clone(), meta.success_rate));
        }

        // Check slow: avg_duration_ms > 10_000.
        if meta.avg_duration_ms > 10_000 {
            slow.push((schema.name.clone(), meta.avg_duration_ms));
        }
    }

    // Build report.
    let mut report = String::from("<b>Monthly Tool Review</b>\n\n");

    if unused.is_empty() && failing.is_empty() && slow.is_empty() {
        report.push_str("All tools are healthy. No issues found.");
        info!(
            event = "tool_review",
            issues = 0,
            "tool review: all healthy"
        );
    } else {
        if !unused.is_empty() {
            report.push_str("<b>Unused tools</b> (no activity in 30+ days):\n");
            for name in &unused {
                report.push_str(&format!("  - <code>{}</code>\n", escape_html(name)));
            }
            report.push('\n');
        }

        if !failing.is_empty() {
            report.push_str("<b>Failing tools</b> (success rate &lt; 70%):\n");
            for (name, rate) in &failing {
                report.push_str(&format!(
                    "  - <code>{}</code> ({:.0}% success)\n",
                    escape_html(name),
                    rate * 100.0
                ));
            }
            report.push('\n');
        }

        if !slow.is_empty() {
            report.push_str("<b>Slow tools</b> (avg &gt; 10s):\n");
            #[allow(clippy::cast_precision_loss)]
            for (name, ms) in &slow {
                let secs = *ms as f64 / 1000.0;
                report.push_str(&format!(
                    "  - <code>{}</code> (avg {secs:.1}s)\n",
                    escape_html(name),
                ));
            }
            report.push('\n');
        }

        let total_issues = unused
            .len()
            .saturating_add(failing.len())
            .saturating_add(slow.len());
        info!(
            event = "tool_review",
            issues = total_issues,
            unused = unused.len(),
            failing = failing.len(),
            slow = slow.len(),
            "tool review complete"
        );
    }

    // Send report via Telegram.
    let msg = TelegramOutbound {
        user_id,
        text: Some(report.clone()),
        file_path: None,
        approval_keyboard: None,
    };
    telegram_tx.send(msg).await?;

    Ok(report)
}
