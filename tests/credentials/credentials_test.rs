//! Coverage for credential loading, permission checks, and debug redaction.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use wintermute::credentials::{enforce_private_file_permissions, load_credentials, Credentials};

fn temp_env_path() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wintermute_test_{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("temp dir should be creatable");
    dir.join(".env")
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

#[test]
fn loads_env_credentials() {
    let env_path = temp_env_path();
    fs::write(
        &env_path,
        "WINTERMUTE_TELEGRAM_TOKEN=test-token\nANTHROPIC_API_KEY=abc123\n",
    )
    .expect("write should succeed");
    enforce_private_file_permissions(&env_path).expect("permissions should set");

    let credentials = load_credentials(&env_path).expect("credentials should load");
    assert_eq!(
        credentials.get("WINTERMUTE_TELEGRAM_TOKEN"),
        Some("test-token")
    );
    assert_eq!(credentials.get("ANTHROPIC_API_KEY"), Some("abc123"));
}

#[test]
fn missing_key_returns_none() {
    let credentials = Credentials::default();
    assert!(credentials.get("NONEXISTENT").is_none());
}

#[test]
fn require_returns_error_on_missing_key() {
    let credentials = Credentials::default();
    let result = credentials.require("MISSING_KEY");
    assert!(result.is_err());
}

#[test]
fn require_returns_value_when_present() {
    let mut vars = BTreeMap::new();
    vars.insert("API_KEY".to_owned(), "secret".to_owned());
    let credentials = Credentials::from_map(vars);
    let result = credentials.require("API_KEY");
    assert_eq!(result.expect("should be present"), "secret");
}

#[test]
fn known_secrets_excludes_empty_values() {
    let mut vars = BTreeMap::new();
    vars.insert("EMPTY".to_owned(), "".to_owned());
    vars.insert("SPACES".to_owned(), "   ".to_owned());
    vars.insert("REAL".to_owned(), "secret-value".to_owned());
    let credentials = Credentials::from_map(vars);
    let secrets = credentials.known_secrets();
    assert_eq!(secrets.len(), 1);
    assert_eq!(secrets[0], "secret-value");
}

// ---------------------------------------------------------------------------
// Debug redaction
// ---------------------------------------------------------------------------

#[test]
fn debug_format_redacts_values() {
    let mut vars = BTreeMap::new();
    vars.insert("API_KEY".to_owned(), "super-secret".to_owned());
    let credentials = Credentials::from_map(vars);
    let debug_output = format!("{credentials:?}");
    assert!(!debug_output.contains("super-secret"));
    assert!(debug_output.contains("[REDACTED]"));
    assert!(debug_output.contains("API_KEY"));
}

#[test]
fn debug_format_shows_keys() {
    let mut vars = BTreeMap::new();
    vars.insert("KEY_A".to_owned(), "val_a".to_owned());
    vars.insert("KEY_B".to_owned(), "val_b".to_owned());
    let credentials = Credentials::from_map(vars);
    let debug_output = format!("{credentials:?}");
    assert!(debug_output.contains("KEY_A"));
    assert!(debug_output.contains("KEY_B"));
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn rejects_world_readable_env_file() {
    use std::os::unix::fs::PermissionsExt;

    let env_path = temp_env_path();
    fs::write(
        &env_path,
        "WINTERMUTE_TELEGRAM_TOKEN=test-token\nANTHROPIC_API_KEY=abc123\n",
    )
    .expect("write should succeed");

    fs::set_permissions(&env_path, fs::Permissions::from_mode(0o644))
        .expect("permissions should set");

    let loaded = load_credentials(&env_path);
    assert!(loaded.is_err());
}

#[test]
fn error_on_nonexistent_file() {
    let result = load_credentials(&PathBuf::from("/tmp/nonexistent_wintermute_env"));
    assert!(result.is_err());
}
