//! Coverage for credential loading and permission checks.

use std::fs;
use std::path::PathBuf;

use wintermute::credentials::{enforce_private_file_permissions, load_credentials};

fn temp_env_path() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wintermute_test_{}", uuid::Uuid::new_v4()));
    let create = fs::create_dir_all(&dir);
    assert!(create.is_ok());
    dir.join(".env")
}

#[test]
fn loads_env_credentials() {
    let env_path = temp_env_path();
    let write = fs::write(
        &env_path,
        "WINTERMUTE_TELEGRAM_TOKEN=test-token\nANTHROPIC_API_KEY=abc123\n",
    );
    assert!(write.is_ok());
    let perms = enforce_private_file_permissions(&env_path);
    assert!(perms.is_ok());

    let loaded = load_credentials(&env_path);
    assert!(loaded.is_ok());
    let credentials = match loaded {
        Ok(credentials) => credentials,
        Err(err) => panic!("credentials should load: {err}"),
    };

    assert_eq!(
        credentials.get("WINTERMUTE_TELEGRAM_TOKEN"),
        Some("test-token")
    );
    assert_eq!(credentials.get("ANTHROPIC_API_KEY"), Some("abc123"));
}

#[cfg(unix)]
#[test]
fn rejects_world_readable_env_file() {
    use std::os::unix::fs::PermissionsExt;

    let env_path = temp_env_path();
    let write = fs::write(
        &env_path,
        "WINTERMUTE_TELEGRAM_TOKEN=test-token\nANTHROPIC_API_KEY=abc123\n",
    );
    assert!(write.is_ok());

    let perms = fs::set_permissions(&env_path, fs::Permissions::from_mode(0o644));
    assert!(perms.is_ok());

    let loaded = load_credentials(&env_path);
    assert!(loaded.is_err());
}
