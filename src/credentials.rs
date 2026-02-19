//! Credential loading from runtime `.env`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Context;

use crate::config::runtime_paths;

/// Runtime credentials loaded from the `.env` file.
#[derive(Debug, Clone, Default)]
pub struct Credentials {
    vars: BTreeMap<String, String>,
}

impl Credentials {
    /// Build credentials from a key-value map.
    pub fn from_map(vars: BTreeMap<String, String>) -> Self {
        Self { vars }
    }

    /// Returns a credential value for a key, if present.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    /// Returns a required credential or an error when missing.
    ///
    /// # Errors
    ///
    /// Returns an error when the key does not exist in loaded credentials.
    pub fn require(&self, key: &str) -> anyhow::Result<String> {
        self.vars
            .get(key)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing required credential: {key}"))
    }

    /// Returns all non-empty credential values for redaction purposes.
    pub fn known_secrets(&self) -> Vec<String> {
        self.vars
            .values()
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .collect()
    }
}

/// Load credentials from a specific `.env` path.
///
/// # Errors
///
/// Returns an error if the file does not exist, permissions are too broad,
/// or parsing fails.
pub fn load_credentials(path: &Path) -> anyhow::Result<Credentials> {
    if !path.exists() {
        return Err(anyhow::anyhow!(
            "credentials file does not exist: {}",
            path.display()
        ));
    }

    validate_private_permissions(path)?;

    let mut vars = BTreeMap::new();
    let iter = dotenvy::from_path_iter(path)
        .with_context(|| format!("failed to read credentials at {}", path.display()))?;

    for item in iter {
        let (key, value) = item.with_context(|| {
            format!(
                "failed to parse key-value entry in credentials file {}",
                path.display()
            )
        })?;
        vars.insert(key, value);
    }

    Ok(Credentials { vars })
}

/// Load credentials from `~/.wintermute/.env`.
///
/// # Errors
///
/// Returns an error when runtime paths cannot be resolved or the credentials
/// file is invalid.
pub fn load_default_credentials() -> anyhow::Result<Credentials> {
    let paths = runtime_paths()?;
    load_credentials(&paths.env_file)
}

/// Ensure a file exists and has private permissions when supported.
///
/// # Errors
///
/// Returns an error if metadata cannot be read or permissions cannot be updated.
pub fn enforce_private_file_permissions(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, perms)
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }

    Ok(())
}

#[cfg(unix)]
fn validate_private_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect credentials file {}", path.display()))?;
    let mode = metadata.permissions().mode() & 0o777;

    if mode & 0o077 != 0 {
        return Err(anyhow::anyhow!(
            "credentials file {} must be 0600, found {:o}",
            path.display(),
            mode
        ));
    }

    Ok(())
}

#[cfg(not(unix))]
fn validate_private_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}
