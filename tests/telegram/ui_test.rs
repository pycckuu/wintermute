//! Telegram UI formatting tests.

use wintermute::telegram::ui::{approval_keyboard, escape_html, format_budget, format_tool_call};

#[test]
fn escape_html_escapes_special_chars() {
    assert_eq!(escape_html("<b>test</b>"), "&lt;b&gt;test&lt;/b&gt;");
    assert_eq!(escape_html("a & b"), "a &amp; b");
}

#[test]
fn escape_html_passes_normal_text() {
    let text = "just a normal message";
    assert_eq!(escape_html(text), text);
}

#[test]
fn approval_keyboard_has_two_buttons_with_correct_callbacks() {
    let kb = approval_keyboard("abc12345");
    let rows = &kb.inline_keyboard;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 2);

    // First button: Approve
    let approve = &rows[0][0];
    assert!(approve.text.contains("Approve"));
    match &approve.kind {
        teloxide::types::InlineKeyboardButtonKind::CallbackData(data) => {
            assert_eq!(data, "a:abc12345");
        }
        _ => panic!("expected CallbackData"),
    }

    // Second button: Deny
    let deny = &rows[0][1];
    assert!(deny.text.contains("Deny"));
    match &deny.kind {
        teloxide::types::InlineKeyboardButtonKind::CallbackData(data) => {
            assert_eq!(data, "d:abc12345");
        }
        _ => panic!("expected CallbackData"),
    }
}

#[test]
fn format_tool_call_produces_html_with_tool_name_and_input() {
    let input = serde_json::json!({"command": "ls -la"});
    let html = format_tool_call("execute_command", &input);
    assert!(html.contains("execute_command"), "should contain tool name");
    assert!(html.contains("ls -la"), "should contain input value");
}

#[test]
fn format_tool_call_escapes_html_in_tool_name() {
    let input = serde_json::json!({});
    let html = format_tool_call("<script>alert</script>", &input);
    assert!(
        !html.contains("<script>"),
        "should escape HTML in tool name"
    );
    assert!(
        html.contains("&lt;script&gt;"),
        "should have escaped HTML entities"
    );
}

#[test]
fn format_budget_produces_html_with_values() {
    let html = format_budget(1000, 5000, 10000, 100000);
    assert!(html.contains("1000"));
    assert!(html.contains("5000"));
    assert!(html.contains("10000"));
    assert!(html.contains("100000"));
    assert!(html.contains("<b>Budget</b>"));
}
