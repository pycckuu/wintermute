//! Credential loading from runtime `.env` and OAuth token refresh.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Context;
use serde::Deserialize;
use tracing::{debug, info};

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
    let paths = crate::config::runtime_paths()?;
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
    /// OAuth Bearer token from `.env`.
    OAuth {
        /// The access token sent as `Authorization: Bearer`.
        access_token: String,
        /// Refresh token for obtaining new access tokens on expiry.
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

// ---------------------------------------------------------------------------
// OpenAI OAuth / API key
// ---------------------------------------------------------------------------

/// How Wintermute authenticates with the OpenAI API.
#[derive(Clone, PartialEq, Eq)]
pub enum OpenAiAuth {
    /// OAuth token sent as `Authorization: Bearer`.
    OAuthToken(String),
    /// API key sent as `Authorization: Bearer`.
    ApiKey(String),
}

impl std::fmt::Debug for OpenAiAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OAuthToken(_) => f.debug_tuple("OAuthToken").field(&"[REDACTED]").finish(),
            Self::ApiKey(_) => f.debug_tuple("ApiKey").field(&"[REDACTED]").finish(),
        }
    }
}

impl OpenAiAuth {
    /// Returns the bearer token string regardless of auth variant.
    pub fn token(&self) -> &str {
        match self {
            Self::OAuthToken(token) | Self::ApiKey(token) => token,
        }
    }

    /// Returns secret values for redactor registration.
    pub fn secret_values(&self) -> Vec<String> {
        vec![self.token().to_owned()]
    }
}

/// Resolve OpenAI authentication from loaded `.env` credentials.
///
/// Resolution order:
/// 1. `OPENAI_OAUTH_TOKEN`
/// 2. `OPENAI_API_KEY`
///
/// Returns `None` if no credential source provides a value.
pub fn resolve_openai_auth(credentials: &Credentials) -> Option<OpenAiAuth> {
    if let Some(token) = credentials.get("OPENAI_OAUTH_TOKEN") {
        if !token.trim().is_empty() {
            debug!("using OPENAI_OAUTH_TOKEN from .env");
            return Some(OpenAiAuth::OAuthToken(token.to_owned()));
        }
    }

    if let Some(key) = credentials.get("OPENAI_API_KEY") {
        if !key.trim().is_empty() {
            debug!("using OPENAI_API_KEY from .env");
            return Some(OpenAiAuth::ApiKey(key.to_owned()));
        }
    }

    None
}

/// Resolve Anthropic authentication from `.env` credentials.
///
/// Resolution order:
/// 1. `ANTHROPIC_OAUTH_TOKEN` (+ optional `ANTHROPIC_OAUTH_REFRESH_TOKEN`,
///    `ANTHROPIC_OAUTH_EXPIRES_AT`) from loaded `.env` credentials
/// 2. `ANTHROPIC_API_KEY` from loaded `.env` credentials
///
/// Returns `None` if no credential source provides a value.
pub fn resolve_anthropic_auth(credentials: &Credentials) -> Option<AnthropicAuth> {
    // 1. Try ANTHROPIC_OAUTH_TOKEN from .env
    if let Some(token) = credentials.get("ANTHROPIC_OAUTH_TOKEN") {
        if !token.trim().is_empty() {
            let refresh_token = credentials
                .get("ANTHROPIC_OAUTH_REFRESH_TOKEN")
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned);
            let expires_at = credentials
                .get("ANTHROPIC_OAUTH_EXPIRES_AT")
                .and_then(|s| s.trim().parse::<i64>().ok());
            debug!("using ANTHROPIC_OAUTH_TOKEN from .env");
            return Some(AnthropicAuth::OAuth {
                access_token: token.to_owned(),
                refresh_token,
                expires_at,
            });
        }
    }

    // 2. Try ANTHROPIC_API_KEY from .env
    if let Some(key) = credentials.get("ANTHROPIC_API_KEY") {
        if !key.trim().is_empty() {
            debug!("using ANTHROPIC_API_KEY from .env");
            return Some(AnthropicAuth::ApiKey(key.to_owned()));
        }
    }

    None
}

// ---------------------------------------------------------------------------
// OAuth token refresh
// ---------------------------------------------------------------------------

/// Anthropic OAuth token endpoint used for refresh grants.
const ANTHROPIC_OAUTH_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";

/// Client ID registered for Claude CLI OAuth flows.
const CLAUDE_CLI_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Buffer before actual expiry at which we consider a token expired (60 s).
const EXPIRY_BUFFER_MS: i64 = 60_000;

/// Errors that can occur during credential refresh.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// No refresh token is available to obtain a new access token.
    #[error("no refresh token available")]
    NoRefreshToken,
    /// The OAuth token endpoint returned an error.
    #[error("token refresh failed: {0}")]
    RefreshFailed(String),
    /// Network-level failure during the refresh request.
    #[error("refresh request failed: {0}")]
    Network(#[from] reqwest::Error),
}

/// Response from the Anthropic OAuth token endpoint.
#[derive(Debug, Deserialize)]
struct OAuthRefreshResponse {
    access_token: String,
    expires_in: i64,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// Returns `true` when an OAuth token is expired or expires within 60 seconds.
///
/// Returns `false` for API keys or when `expires_at` is unknown.
#[must_use]
pub fn is_token_expired(auth: &AnthropicAuth) -> bool {
    if let AnthropicAuth::OAuth {
        expires_at: Some(exp),
        ..
    } = auth
    {
        let now_ms = chrono::Utc::now().timestamp_millis();
        *exp <= now_ms.saturating_add(EXPIRY_BUFFER_MS)
    } else {
        false
    }
}

/// Attempt to refresh an expired Anthropic OAuth token.
///
/// Sends a `grant_type=refresh_token` POST to the Anthropic OAuth endpoint.
/// On success returns a new [`AnthropicAuth::OAuth`] with updated tokens.
///
/// # Errors
///
/// Returns [`CredentialError`] when no refresh token is available, the
/// endpoint returns an error, or a network failure occurs.
pub async fn refresh_anthropic_token(
    auth: &AnthropicAuth,
) -> Result<AnthropicAuth, CredentialError> {
    let old_refresh = match auth {
        AnthropicAuth::OAuth {
            refresh_token: Some(rt),
            ..
        } if !rt.is_empty() => rt,
        _ => return Err(CredentialError::NoRefreshToken),
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(CredentialError::Network)?;
    let response = client
        .post(ANTHROPIC_OAUTH_TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", old_refresh),
            ("client_id", CLAUDE_CLI_CLIENT_ID),
        ])
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        // Truncate body to avoid leaking sensitive data in logs.
        let safe_len = body.len().min(200);
        return Err(CredentialError::RefreshFailed(format!(
            "HTTP {status}: {}",
            &body[..safe_len]
        )));
    }

    let parsed: OAuthRefreshResponse = serde_json::from_str(&body)
        .map_err(|e| CredentialError::RefreshFailed(format!("parse error: {e}")))?;

    let now_ms = chrono::Utc::now().timestamp_millis();
    let expires_at = now_ms.saturating_add(parsed.expires_in.saturating_mul(1000));

    // Keep the old refresh token if the server did not issue a new one.
    let new_refresh = parsed.refresh_token.unwrap_or_else(|| old_refresh.clone());

    Ok(AnthropicAuth::OAuth {
        access_token: parsed.access_token,
        refresh_token: Some(new_refresh),
        expires_at: Some(expires_at),
    })
}

/// Persist refreshed OAuth tokens back into a `.env` file.
///
/// Reads the existing file, replaces the three `ANTHROPIC_OAUTH_*` lines, and
/// writes it back. Private permissions are preserved.
///
/// # Errors
///
/// Returns an error if the file cannot be read or written.
pub fn update_env_credentials(env_path: &Path, auth: &AnthropicAuth) -> anyhow::Result<()> {
    let (access_token, refresh_token, expires_at) = match auth {
        AnthropicAuth::OAuth {
            access_token,
            refresh_token,
            expires_at,
        } => (access_token, refresh_token, expires_at),
        AnthropicAuth::ApiKey(_) => return Ok(()),
    };

    let content = fs::read_to_string(env_path)
        .with_context(|| format!("cannot read {}", env_path.display()))?;

    let mut lines: Vec<String> = content.lines().map(str::to_owned).collect();

    upsert_env_line(&mut lines, "ANTHROPIC_OAUTH_TOKEN", access_token);
    if let Some(rt) = refresh_token {
        upsert_env_line(&mut lines, "ANTHROPIC_OAUTH_REFRESH_TOKEN", rt);
    }
    if let Some(exp) = expires_at {
        upsert_env_line(&mut lines, "ANTHROPIC_OAUTH_EXPIRES_AT", &exp.to_string());
    }

    // Ensure trailing newline
    let mut output = lines.join("\n");
    if !output.ends_with('\n') {
        output.push('\n');
    }

    // Atomic write: write to a temporary file in the same directory, then rename.
    let parent = env_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cannot determine parent dir of {}", env_path.display()))?;
    let tmp_path = parent.join(".env.tmp");
    fs::write(&tmp_path, &output)
        .with_context(|| format!("cannot write {}", tmp_path.display()))?;
    enforce_private_file_permissions(&tmp_path)?;
    fs::rename(&tmp_path, env_path).with_context(|| {
        format!(
            "cannot rename {} to {}",
            tmp_path.display(),
            env_path.display()
        )
    })?;

    info!(path = %env_path.display(), "persisted refreshed OAuth tokens to .env");
    Ok(())
}

/// Insert or replace all `KEY=value` lines in a list of `.env` lines.
///
/// Every line starting with `KEY=` or `# KEY=` is replaced with the new value.
/// If no matching line exists, a new line is appended.
fn upsert_env_line(lines: &mut Vec<String>, key: &str, value: &str) {
    let prefix_active = format!("{key}=");
    let prefix_comment = format!("# {key}=");

    let mut found = false;
    for line in lines.iter_mut() {
        if line.starts_with(&prefix_active) || line.starts_with(&prefix_comment) {
            *line = format!("{key}={value}");
            found = true;
        }
    }
    if !found {
        lines.push(format!("{key}={value}"));
    }
}
