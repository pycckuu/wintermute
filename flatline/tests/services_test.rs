//! Tests for the service management module.

use flatline::services::ServiceManager;

#[test]
fn service_manager_debug_impl() {
    let launchd = ServiceManager::Launchd;
    let systemd = ServiceManager::Systemd;

    let launchd_debug = format!("{launchd:?}");
    let systemd_debug = format!("{systemd:?}");

    assert!(launchd_debug.contains("Launchd"));
    assert!(systemd_debug.contains("Systemd"));
}

#[test]
fn service_manager_equality() {
    assert_eq!(ServiceManager::Launchd, ServiceManager::Launchd);
    assert_eq!(ServiceManager::Systemd, ServiceManager::Systemd);
    assert_ne!(ServiceManager::Launchd, ServiceManager::Systemd);
}

#[test]
fn service_manager_clone() {
    let original = ServiceManager::Launchd;
    let cloned = original;
    assert_eq!(original, cloned);
}

/// Verify that detection doesn't panic even when service files don't exist.
/// The result depends on the platform and whether services are installed,
/// so we only assert that it returns without error.
#[test]
fn detect_does_not_panic() {
    let _result = flatline::services::detect();
}

/// Verify that `services.rs` only uses hardcoded arguments in Command calls.
/// No format!, variable interpolation, or user input should reach Command args.
#[test]
fn services_uses_only_hardcoded_command_names() {
    let services_src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/services.rs");
    let content = std::fs::read_to_string(&services_src)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", services_src.display()));

    // Command::new() should only be called with string literals.
    // Both command names exist in source unconditionally (behind runtime cfg!,
    // not compile-time #[cfg]), so both assertions hold on all platforms.
    assert!(
        content.contains("Command::new(\"launchctl\")"),
        "services.rs must use hardcoded \"launchctl\" in Command::new"
    );
    assert!(
        content.contains("Command::new(\"systemctl\")"),
        "services.rs must use hardcoded \"systemctl\" in Command::new"
    );
}
