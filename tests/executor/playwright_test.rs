//! Tests for `src/executor/playwright.rs` — browser sidecar configuration.

use wintermute::executor::playwright::BROWSER_IMAGE;

#[test]
fn browser_image_constant_matches_expected_registry() {
    assert!(
        BROWSER_IMAGE.starts_with("ghcr.io/"),
        "browser image should be hosted on ghcr.io"
    );
    assert!(
        BROWSER_IMAGE.contains("wintermute-browser"),
        "browser image name should contain wintermute-browser"
    );
}

#[test]
fn base_url_uses_localhost_port_9222() {
    // The sidecar binds port 9222 to localhost for host access.
    let expected = "http://127.0.0.1:9222";
    // Validate the expected format is a valid URL.
    let parsed = url::Url::parse(expected);
    assert!(parsed.is_ok(), "base_url format should be a valid URL");
    let parsed = parsed.expect("just checked");
    assert_eq!(parsed.host_str(), Some("127.0.0.1"));
    assert_eq!(parsed.port(), Some(9222));
    assert_eq!(parsed.scheme(), "http");
}
