/// Vault abstraction for secret storage (spec 6.4).
///
/// Only kernel code can access the vault directly. Tools receive
/// `InjectedCredentials`, never vault references. Phase 1 uses an
/// in-memory implementation; SQLCipher with OS keychain master key
/// will be added in Phase 2.
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::RwLock;

/// Vault error types (spec 6.4).
#[derive(Debug, Error)]
pub enum VaultError {
    /// The requested secret reference ID does not exist.
    #[error("secret not found: {0}")]
    NotFound(String),
    /// A storage or access error occurred.
    #[error("vault access error: {0}")]
    AccessError(String),
}

/// Opaque secret value that never appears in logs (spec 6.4).
///
/// Debug output always shows `__REDACTED__` to prevent accidental
/// secret leakage in logs, error messages, or debug output.
#[derive(Clone)]
pub struct SecretValue(String);

impl SecretValue {
    /// Create a new secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Expose the secret value. Use only when injecting credentials to tools.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("__REDACTED__")
    }
}

/// Trait for secret storage (spec 6.4).
///
/// Only kernel code accesses this directly. Tools receive resolved
/// credentials via `InjectedCredentials`, never vault references.
#[async_trait]
pub trait SecretStore: Send + Sync {
    /// Store a secret by reference ID. Overwrites any existing value.
    async fn store_secret(&self, ref_id: &str, value: SecretValue) -> Result<(), VaultError>;

    /// Retrieve a secret by reference ID.
    async fn get_secret(&self, ref_id: &str) -> Result<SecretValue, VaultError>;

    /// Delete a secret by reference ID.
    async fn delete_secret(&self, ref_id: &str) -> Result<(), VaultError>;

    /// List all secret reference IDs (values are never returned).
    async fn list_secrets(&self) -> Result<Vec<String>, VaultError>;
}

/// In-memory vault for Phase 1 testing (spec 6.4).
///
/// Production will use SQLCipher with OS keychain master key.
pub struct InMemoryVault {
    secrets: Arc<RwLock<HashMap<String, SecretValue>>>,
}

impl InMemoryVault {
    /// Create an empty in-memory vault.
    pub fn new() -> Self {
        Self {
            secrets: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for InMemoryVault {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecretStore for InMemoryVault {
    async fn store_secret(&self, ref_id: &str, value: SecretValue) -> Result<(), VaultError> {
        let mut secrets = self.secrets.write().await;
        secrets.insert(ref_id.to_owned(), value);
        Ok(())
    }

    async fn get_secret(&self, ref_id: &str) -> Result<SecretValue, VaultError> {
        let secrets = self.secrets.read().await;
        secrets
            .get(ref_id)
            .cloned()
            .ok_or_else(|| VaultError::NotFound(ref_id.to_owned()))
    }

    async fn delete_secret(&self, ref_id: &str) -> Result<(), VaultError> {
        let mut secrets = self.secrets.write().await;
        secrets
            .remove(ref_id)
            .map(|_| ())
            .ok_or_else(|| VaultError::NotFound(ref_id.to_owned()))
    }

    async fn list_secrets(&self) -> Result<Vec<String>, VaultError> {
        let secrets = self.secrets.read().await;
        let mut keys: Vec<String> = secrets.keys().cloned().collect();
        keys.sort();
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_store_and_get() {
        let vault = InMemoryVault::new();
        vault
            .store_secret("api_key", SecretValue::new("sk-12345"))
            .await
            .expect("store should succeed");

        let secret = vault
            .get_secret("api_key")
            .await
            .expect("get should succeed");
        assert_eq!(secret.expose(), "sk-12345");
    }

    #[tokio::test]
    async fn test_get_not_found() {
        let vault = InMemoryVault::new();
        let result = vault.get_secret("nonexistent").await;
        assert!(matches!(result, Err(VaultError::NotFound(ref id)) if id == "nonexistent"));
    }

    #[tokio::test]
    async fn test_delete() {
        let vault = InMemoryVault::new();
        vault
            .store_secret("temp", SecretValue::new("val"))
            .await
            .expect("store");
        vault.delete_secret("temp").await.expect("delete");
        assert!(matches!(
            vault.get_secret("temp").await,
            Err(VaultError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn test_list_secrets() {
        let vault = InMemoryVault::new();
        vault
            .store_secret("beta", SecretValue::new("b"))
            .await
            .expect("store");
        vault
            .store_secret("alpha", SecretValue::new("a"))
            .await
            .expect("store");
        let keys = vault.list_secrets().await.expect("list");
        assert_eq!(keys, vec!["alpha", "beta"]);
    }

    #[tokio::test]
    async fn test_secret_debug_redacted() {
        let secret = SecretValue::new("super-secret");
        assert_eq!(format!("{secret:?}"), "__REDACTED__");
    }

    #[tokio::test]
    async fn test_overwrite_secret() {
        let vault = InMemoryVault::new();
        vault
            .store_secret("k", SecretValue::new("v1"))
            .await
            .expect("store");
        vault
            .store_secret("k", SecretValue::new("v2"))
            .await
            .expect("overwrite");
        let s = vault.get_secret("k").await.expect("get");
        assert_eq!(s.expose(), "v2");
    }
}
