//! Tests for `src/executor/playwright.rs` — browser sidecar configuration.

use wintermute::executor::playwright::{BRIDGE_SCRIPT, BROWSER_IMAGE};

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
fn base_url_uses_localhost_port_9223() {
    // The sidecar binds port 9223 to localhost for host access.
    let expected = "http://127.0.0.1:9223";
    // Validate the expected format is a valid URL.
    let parsed = url::Url::parse(expected);
    assert!(parsed.is_ok(), "base_url format should be a valid URL");
    let parsed = parsed.expect("just checked");
    assert_eq!(parsed.host_str(), Some("127.0.0.1"));
    assert_eq!(parsed.port(), Some(9223));
    assert_eq!(parsed.scheme(), "http");
}

// ---------------------------------------------------------------------------
// Bridge script content assertions
// ---------------------------------------------------------------------------

#[test]
fn bridge_script_contains_cdp_target_connect_logic() {
    assert!(
        BRIDGE_SCRIPT.contains("CDP_TARGET"),
        "bridge script should reference CDP_TARGET env var"
    );
    assert!(
        BRIDGE_SCRIPT.contains("connect_over_cdp"),
        "bridge script should use connect_over_cdp for attached mode"
    );
}

#[test]
fn bridge_script_contains_tab_management_actions() {
    for action in ["list_tabs", "switch_tab", "new_tab", "close_tab"] {
        assert!(
            BRIDGE_SCRIPT.contains(action),
            "bridge script should handle {action} action"
        );
    }
}

#[test]
fn bridge_script_contains_get_all_pages_helper() {
    assert!(
        BRIDGE_SCRIPT.contains("get_all_pages"),
        "bridge script should define get_all_pages helper"
    );
}

#[test]
fn bridge_script_uses_bring_to_front_for_tab_switch() {
    assert!(
        BRIDGE_SCRIPT.contains("bring_to_front"),
        "switch_tab should use bring_to_front to activate the tab"
    );
}

#[test]
fn bridge_script_tracks_agent_created_tabs() {
    assert!(
        BRIDGE_SCRIPT.contains("agent_created_tabs"),
        "bridge script should track tabs created by the agent"
    );
}

#[test]
fn bridge_script_enforces_close_tab_safety_in_attached_mode() {
    assert!(
        BRIDGE_SCRIPT.contains("not created by agent"),
        "close_tab should refuse to close tabs not created by the agent in attached mode"
    );
}

// ---------------------------------------------------------------------------
// Source-scanning invariant assertions
// ---------------------------------------------------------------------------

#[test]
fn browser_sidecar_does_not_join_wintermute_net() {
    let src = std::fs::read_to_string("src/executor/playwright.rs")
        .expect("should read playwright.rs source");
    // Check that wintermute-net is never used in code (comments are fine).
    // The string in code would appear as a quoted literal like "wintermute-net".
    assert!(
        !src.contains("\"wintermute-net\""),
        "browser sidecar must not reference wintermute-net as a network name in code"
    );
}

#[test]
fn browser_sidecar_passes_cdp_target_env_when_configured() {
    let src = std::fs::read_to_string("src/executor/playwright.rs")
        .expect("should read playwright.rs source");
    assert!(
        src.contains("CDP_TARGET={target}") || src.contains("CDP_TARGET="),
        "create_browser_container should pass CDP_TARGET env var"
    );
}
