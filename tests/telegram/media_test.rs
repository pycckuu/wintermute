//! Tests for `src/telegram/media.rs` — filename sanitization and description types.
//!
//! Note: `handle_voice`, `handle_photo`, and `handle_document` require a live
//! Telegram Bot connection and cannot be unit-tested without mocking teloxide.

use wintermute::telegram::media::{sanitize_filename, MediaDescription};

#[test]
fn sanitize_strips_path_separators() {
    // "../../../.env" → replace / with _ → ".._.._.._.env" → trim leading . → "_.._.._.env"
    assert_eq!(sanitize_filename("../../../.env"), "_.._.._.env");
}

#[test]
fn sanitize_strips_backslash_separators() {
    // "..\\..\\secret.txt" → replace \ with _ → ".._.._secret.txt" → trim leading dots → "_.._secret.txt"
    assert_eq!(sanitize_filename("..\\..\\secret.txt"), "_.._secret.txt");
}

#[test]
fn sanitize_strips_leading_dots() {
    assert_eq!(sanitize_filename(".hidden"), "hidden");
    assert_eq!(sanitize_filename("...triple"), "triple");
}

#[test]
fn sanitize_preserves_normal_filename() {
    assert_eq!(sanitize_filename("report.pdf"), "report.pdf");
    assert_eq!(sanitize_filename("my_file.txt"), "my_file.txt");
}

#[test]
fn sanitize_returns_fallback_for_empty() {
    let result = sanitize_filename("");
    assert!(
        result.starts_with("doc_"),
        "expected fallback name, got: {result}"
    );
}

#[test]
fn sanitize_returns_fallback_for_dots_only() {
    let result = sanitize_filename("...");
    assert!(
        result.starts_with("doc_"),
        "expected fallback name, got: {result}"
    );
}

#[test]
fn sanitize_handles_slash_only_filename() {
    // "/" becomes "_", then trim leading dots (no dots), so "_"
    assert_eq!(sanitize_filename("/"), "_");
}

#[test]
fn media_description_is_debug() {
    let desc = MediaDescription {
        text: "[Photo: /workspace/inbox/photo.jpg]".to_owned(),
        file_path: std::path::PathBuf::from("/workspace/inbox/photo.jpg"),
    };
    let debug_str = format!("{desc:?}");
    assert!(debug_str.contains("MediaDescription"));
}
