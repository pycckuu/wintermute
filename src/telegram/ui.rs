//! HTML formatting and inline keyboard helpers for Telegram messages.
//!
//! All output uses HTML parse mode (never MarkdownV2) per project convention.

use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};

/// Escape special HTML characters in user-provided text.
pub fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Build an inline keyboard with Approve and Deny buttons for a given approval ID.
pub fn approval_keyboard(approval_id: &str) -> InlineKeyboardMarkup {
    let approve =
        InlineKeyboardButton::callback("\u{2705} Approve".to_owned(), format!("a:{approval_id}"));
    let deny =
        InlineKeyboardButton::callback("\u{274C} Deny".to_owned(), format!("d:{approval_id}"));
    InlineKeyboardMarkup::new(vec![vec![approve, deny]])
}

/// Format a tool call description as HTML.
pub fn format_tool_call(tool_name: &str, input: &serde_json::Value) -> String {
    let escaped_name = escape_html(tool_name);
    let input_str = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
    let escaped_input = escape_html(&input_str);
    format!("<b>Tool:</b> <code>{escaped_name}</code>\n<b>Input:</b>\n<pre>{escaped_input}</pre>")
}

/// Format budget usage as an HTML status message.
pub fn format_budget(
    session_used: u64,
    daily_used: u64,
    session_limit: u64,
    daily_limit: u64,
) -> String {
    format!(
        "<b>Budget</b>\nSession: {session_used} / {session_limit} tokens\nDaily: {daily_used} / {daily_limit} tokens"
    )
}
