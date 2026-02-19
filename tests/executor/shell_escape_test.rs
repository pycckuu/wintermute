//! Shell escaping tests.

use wintermute::executor::docker::shell_escape;

#[test]
fn simple_string_is_quoted() {
    let result = shell_escape("echo hello");
    assert_eq!(result, "'echo hello'");
}

#[test]
fn single_quotes_are_escaped() {
    let result = shell_escape("echo 'hello'");
    assert_eq!(result, r"'echo '\''hello'\'''");
}

#[test]
fn empty_string_produces_empty_quotes() {
    let result = shell_escape("");
    assert_eq!(result, "''");
}

#[test]
fn special_characters_are_preserved() {
    let result = shell_escape("echo $HOME && ls -la");
    assert_eq!(result, "'echo $HOME && ls -la'");
}
