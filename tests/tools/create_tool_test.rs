//! Tests for `src/tools/create_tool.rs` — tool name validation.

use wintermute::tools::create_tool::validate_tool_name;

#[test]
fn accepts_valid_simple_name() {
    assert!(validate_tool_name("news_digest").is_ok());
}

#[test]
fn accepts_valid_name_with_numbers() {
    assert!(validate_tool_name("my_tool").is_ok());
}

#[test]
fn accepts_short_name() {
    assert!(validate_tool_name("a1").is_ok());
}

#[test]
fn accepts_single_letter() {
    assert!(validate_tool_name("x").is_ok());
}

#[test]
fn rejects_name_with_forward_slash() {
    assert!(validate_tool_name("path/traversal").is_err());
}

#[test]
fn rejects_name_with_dot_dot() {
    assert!(validate_tool_name("up..dir").is_err());
}

#[test]
fn rejects_name_with_backslash() {
    assert!(validate_tool_name("back\\slash").is_err());
}

#[test]
fn rejects_name_with_uppercase() {
    assert!(validate_tool_name("MyTool").is_err());
}

#[test]
fn rejects_name_starting_with_number() {
    assert!(validate_tool_name("1tool").is_err());
}

#[test]
fn rejects_name_starting_with_underscore_system() {
    assert!(validate_tool_name("_system_hook").is_err());
}

#[test]
fn rejects_empty_name() {
    assert!(validate_tool_name("").is_err());
}

#[test]
fn rejects_name_too_long() {
    let long_name = "a".repeat(65);
    assert!(validate_tool_name(&long_name).is_err());
}

#[test]
fn accepts_name_at_max_length() {
    let name = "a".repeat(64);
    assert!(validate_tool_name(&name).is_ok());
}

#[test]
fn rejects_name_with_hyphen() {
    assert!(validate_tool_name("my-tool").is_err());
}

#[test]
fn rejects_name_with_space() {
    assert!(validate_tool_name("my tool").is_err());
}

#[test]
fn rejects_name_starting_with_underscore() {
    // Underscore is valid inside a name but not as the first character
    // (it must start with a lowercase letter).
    assert!(validate_tool_name("_private").is_err());
}
