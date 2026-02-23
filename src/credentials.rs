//! Credential loading from runtime `.env` and external OAuth sources.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Context;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::config::runtime_paths;

/// Runtime credentials loaded from the `.env` file.
#[derive(Clone, Default)]
pub struct Credentials {
    vars: BTreeMap<String, String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("keys", &self.vars.keys().collect::<Vec<_>>())
            .field("values", &"[REDACTED]")
            .finish()
    }
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

// ---------------------------------------------------------------------------
// Anthropic OAuth
// ---------------------------------------------------------------------------

/// How Wintermute authenticates with the Anthropic API.
#[derive(Clone, PartialEq, Eq)]
pub enum AnthropicAuth {
    /// OAuth Bearer token (from Claude CLI or env var).
    OAuth {
        /// The access token sent as `Authorization: Bearer`.
        access_token: String,
        /// Optional refresh token (stored for potential future use).
        refresh_token: Option<String>,
        /// Expiry timestamp in milliseconds since epoch. `None` if unknown.
        expires_at: Option<i64>,
    },
    /// Classic API key sent as `x-api-key` header.
    ApiKey(String),
}

impl std::fmt::Debug for AnthropicAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OAuth { expires_at, .. } => f
                .debug_struct("OAuth")
                .field("access_token", &"[REDACTED]")
                .field("refresh_token", &"[REDACTED]")
                .field("expires_at", expires_at)
                .finish(),
            Self::ApiKey(_) => f.debug_tuple("ApiKey").field(&"[REDACTED]").finish(),
        }
    }
}

impl AnthropicAuth {
    /// Returns secret values for redactor registration.
    ///
    /// Includes the access token, refresh token (if present), or API key.
    pub fn secret_values(&self) -> Vec<String> {
        match self {
            Self::OAuth {
                access_token,
                refresh_token,
                ..
            } => {
                let mut secrets = vec![access_token.clone()];
                if let Some(rt) = refresh_token {
                    if !rt.is_empty() {
                        secrets.push(rt.clone());
                    }
                }
                secrets
            }
            Self::ApiKey(key) => vec![key.clone()],
        }
    }
}

/// Resolve Anthropic authentication using a priority chain.
///
/// Resolution order:
/// 1. Claude CLI keychain (macOS only)
/// 2. Claude CLI credentials file (`~/.claude/.credentials.json`)
/// 3. `ANTHROPIC_OAUTH_TOKEN` from loaded `.env` credentials
/// 4. `ANTHROPIC_API_KEY` from loaded `.env` credentials
///
/// Returns `None` if no credential source provides a value.
pub fn resolve_anthropic_auth(credentials: &Credentials) -> Option<AnthropicAuth> {
    // 1. Try macOS keychain
    if let Some(auth) = read_claude_cli_keychain() {
        check_token_expiry(&auth, "keychain");
        return Some(auth);
    }

    // 2. Try ~/.claude/.credentials.json
    if let Some(auth) = read_claude_cli_file() {
        check_token_expiry(&auth, "credentials file");
        return Some(auth);
    }

    // 3. Try ANTHROPIC_OAUTH_TOKEN env
    if let Some(token) = credentials.get("ANTHROPIC_OAUTH_TOKEN") {
        if !token.trim().is_empty() {
            debug!("using ANTHROPIC_OAUTH_TOKEN from .env");
            return Some(AnthropicAuth::OAuth {
                access_token: token.to_owned(),
                refresh_token: None,
                expires_at: None,
            });
        }
    }

    // 4. Try ANTHROPIC_API_KEY env
    if let Some(key) = credentials.get("ANTHROPIC_API_KEY") {
        if !key.trim().is_empty() {
            debug!("using ANTHROPIC_API_KEY from .env");
            return Some(AnthropicAuth::ApiKey(key.to_owned()));
        }
    }

    None
}

/// Parse Claude CLI JSON credentials into an [`AnthropicAuth`].
///
/// Expects the format: `{ "claudeAiOauth": { "accessToken": "...", ... } }`.
/// Exported for testing.
#[doc(hidden)]
pub fn parse_claude_cli_json(json_str: &str) -> Option<AnthropicAuth> {
    let parsed: ClaudeCliCredentials = serde_json::from_str(json_str)
        .map_err(|e| {
            debug!(error = %e, "failed to parse Claude CLI credentials JSON");
        })
        .ok()?;

    let oauth = parsed.claude_ai_oauth?;
    if oauth.access_token.is_empty() {
        return None;
    }

    Some(AnthropicAuth::OAuth {
        access_token: oauth.access_token,
        refresh_token: oauth.refresh_token.filter(|s| !s.is_empty()),
        expires_at: oauth.expires_at,
    })
}

/// Stored OAuth credential structure matching Claude Code's JSON format.
#[derive(Debug, Deserialize)]
struct ClaudeCliCredentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeAiOauth>,
}

/// Inner OAuth fields from Claude Code's credential store.
#[derive(Debug, Deserialize)]
struct ClaudeAiOauth {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>,
}

/// Keychain service name used by Claude Code to store OAuth credentials.
const CLAUDE_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Keychain account name used by Claude Code.
const CLAUDE_KEYCHAIN_ACCOUNT: &str = "Claude Code";

/// Read Claude CLI credentials from macOS Keychain.
#[cfg(target_os = "macos")]
fn read_claude_cli_keychain() -> Option<AnthropicAuth> {
    use security_framework::passwords::get_generic_password;

    let password_bytes = get_generic_password(CLAUDE_KEYCHAIN_SERVICE, CLAUDE_KEYCHAIN_ACCOUNT)
        .map_err(|e| {
            debug!(error = %e, "Claude CLI keychain entry not found");
        })
        .ok()?;

    let json_str = std::str::from_utf8(&password_bytes)
        .map_err(|e| {
            warn!(error = %e, "Claude CLI keychain entry is not valid UTF-8");
        })
        .ok()?;

    let result = parse_claude_cli_json(json_str);
    if result.is_some() {
        debug!("loaded Anthropic OAuth from Claude CLI keychain");
    }
    result
}

/// Keychain is not available on non-macOS platforms.
#[cfg(not(target_os = "macos"))]
fn read_claude_cli_keychain() -> Option<AnthropicAuth> {
    None
}

/// Read Claude CLI credentials from `~/.claude/.credentials.json`.
fn read_claude_cli_file() -> Option<AnthropicAuth> {
    let base_dirs = directories::BaseDirs::new()?;
    let cred_path = base_dirs
        .home_dir()
        .join(".claude")
        .join(".credentials.json");

    let content = fs::read_to_string(&cred_path)
        .map_err(|e| {
            debug!(
                path = %cred_path.display(),
                error = %e,
                "Claude CLI credentials file not readable"
            );
        })
        .ok()?;

    let result = parse_claude_cli_json(&content);
    if result.is_some() {
        debug!(path = %cred_path.display(), "loaded Anthropic OAuth from Claude CLI file");
    }
    result
}

/// Log a warning if an OAuth token is expired or expiring soon.
fn check_token_expiry(auth: &AnthropicAuth, source: &str) {
    if let AnthropicAuth::OAuth {
        expires_at: Some(exp),
        ..
    } = auth
    {
        let now_ms = chrono::Utc::now().timestamp_millis();
        if *exp <= now_ms {
            warn!(
                source,
                expires_at = exp,
                "Anthropic OAuth token has expired; API requests may fail"
            );
        } else if *exp <= now_ms.saturating_add(300_000) {
            warn!(
                source,
                expires_at = exp,
                "Anthropic OAuth token expires within 5 minutes"
            );
        } else {
            debug!(source, expires_at = exp, "Anthropic OAuth token valid");
        }
    }
}
