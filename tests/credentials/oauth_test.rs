//! Tests for Anthropic OAuth credential resolution and parsing.

use std::collections::BTreeMap;

use wintermute::credentials::{
    resolve_anthropic_auth, resolve_openai_auth, AnthropicAuth, Credentials, OpenAiAuth,
};

// ---------------------------------------------------------------------------
// resolve_anthropic_auth
// ---------------------------------------------------------------------------

#[test]
fn oauth_env_priority_over_api_key() {
    let mut vars = BTreeMap::new();
    vars.insert(
        "ANTHROPIC_OAUTH_TOKEN".to_owned(),
        "oauth-token-123".to_owned(),
    );
    vars.insert("ANTHROPIC_API_KEY".to_owned(), "api-key-456".to_owned());
    let credentials = Credentials::from_map(vars);
    let auth = resolve_anthropic_auth(&credentials).expect("should resolve");
    assert!(matches!(
        auth,
        AnthropicAuth::OAuth {
            ref access_token, ..
        } if access_token == "oauth-token-123"
    ));
}

#[test]
fn falls_back_to_api_key() {
    let mut vars = BTreeMap::new();
    vars.insert("ANTHROPIC_API_KEY".to_owned(), "my-key".to_owned());
    let credentials = Credentials::from_map(vars);
    let auth = resolve_anthropic_auth(&credentials).expect("should resolve");
    assert_eq!(auth, AnthropicAuth::ApiKey("my-key".to_owned()));
}

#[test]
fn returns_none_when_empty() {
    let credentials = Credentials::default();
    assert!(resolve_anthropic_auth(&credentials).is_none());
}

#[test]
fn empty_oauth_token_skipped() {
    let mut vars = BTreeMap::new();
    vars.insert("ANTHROPIC_OAUTH_TOKEN".to_owned(), "  ".to_owned());
    vars.insert("ANTHROPIC_API_KEY".to_owned(), "fallback-key".to_owned());
    let credentials = Credentials::from_map(vars);
    let auth = resolve_anthropic_auth(&credentials).expect("should resolve");
    assert_eq!(auth, AnthropicAuth::ApiKey("fallback-key".to_owned()));
}

#[test]
fn oauth_reads_refresh_token_and_expires_at() {
    let mut vars = BTreeMap::new();
    vars.insert("ANTHROPIC_OAUTH_TOKEN".to_owned(), "access-tok".to_owned());
    vars.insert(
        "ANTHROPIC_OAUTH_REFRESH_TOKEN".to_owned(),
        "refresh-tok".to_owned(),
    );
    vars.insert(
        "ANTHROPIC_OAUTH_EXPIRES_AT".to_owned(),
        "9999999999999".to_owned(),
    );
    let credentials = Credentials::from_map(vars);
    let auth = resolve_anthropic_auth(&credentials).expect("should resolve");
    assert_eq!(
        auth,
        AnthropicAuth::OAuth {
            access_token: "access-tok".to_owned(),
            refresh_token: Some("refresh-tok".to_owned()),
            expires_at: Some(9999999999999),
        }
    );
}

#[test]
fn oauth_ignores_empty_refresh_token() {
    let mut vars = BTreeMap::new();
    vars.insert("ANTHROPIC_OAUTH_TOKEN".to_owned(), "access-tok".to_owned());
    vars.insert("ANTHROPIC_OAUTH_REFRESH_TOKEN".to_owned(), "  ".to_owned());
    let credentials = Credentials::from_map(vars);
    let auth = resolve_anthropic_auth(&credentials).expect("should resolve");
    assert_eq!(
        auth,
        AnthropicAuth::OAuth {
            access_token: "access-tok".to_owned(),
            refresh_token: None,
            expires_at: None,
        }
    );
}

#[test]
fn oauth_ignores_invalid_expires_at() {
    let mut vars = BTreeMap::new();
    vars.insert("ANTHROPIC_OAUTH_TOKEN".to_owned(), "access-tok".to_owned());
    vars.insert(
        "ANTHROPIC_OAUTH_EXPIRES_AT".to_owned(),
        "not-a-number".to_owned(),
    );
    let credentials = Credentials::from_map(vars);
    let auth = resolve_anthropic_auth(&credentials).expect("should resolve");
    assert_eq!(
        auth,
        AnthropicAuth::OAuth {
            access_token: "access-tok".to_owned(),
            refresh_token: None,
            expires_at: None,
        }
    );
}

// ---------------------------------------------------------------------------
// AnthropicAuth::secret_values
// ---------------------------------------------------------------------------

#[test]
fn secret_values_covers_all_tokens() {
    let auth = AnthropicAuth::OAuth {
        access_token: "access-tok".to_owned(),
        refresh_token: Some("refresh-tok".to_owned()),
        expires_at: Some(9999999999999),
    };
    let secrets = auth.secret_values();
    assert_eq!(secrets.len(), 2);
    assert!(secrets.contains(&"access-tok".to_owned()));
    assert!(secrets.contains(&"refresh-tok".to_owned()));
}

#[test]
fn secret_values_omits_empty_refresh() {
    let auth = AnthropicAuth::OAuth {
        access_token: "access-tok".to_owned(),
        refresh_token: Some(String::new()),
        expires_at: None,
    };
    let secrets = auth.secret_values();
    assert_eq!(secrets.len(), 1);
    assert_eq!(secrets[0], "access-tok");
}

#[test]
fn api_key_secret_values() {
    let auth = AnthropicAuth::ApiKey("my-key".to_owned());
    let secrets = auth.secret_values();
    assert_eq!(secrets, vec!["my-key"]);
}

// ---------------------------------------------------------------------------
// resolve_openai_auth
// ---------------------------------------------------------------------------

#[test]
fn openai_oauth_priority_over_api_key() {
    let mut vars = BTreeMap::new();
    vars.insert(
        "OPENAI_OAUTH_TOKEN".to_owned(),
        "oauth-token-123".to_owned(),
    );
    vars.insert("OPENAI_API_KEY".to_owned(), "api-key-456".to_owned());
    let credentials = Credentials::from_map(vars);
    let auth = resolve_openai_auth(&credentials).expect("should resolve");
    assert_eq!(auth, OpenAiAuth::OAuthToken("oauth-token-123".to_owned()));
}

#[test]
fn openai_falls_back_to_api_key() {
    let mut vars = BTreeMap::new();
    vars.insert("OPENAI_API_KEY".to_owned(), "my-key".to_owned());
    let credentials = Credentials::from_map(vars);
    let auth = resolve_openai_auth(&credentials).expect("should resolve");
    assert_eq!(auth, OpenAiAuth::ApiKey("my-key".to_owned()));
}

#[test]
fn openai_returns_none_when_missing() {
    let credentials = Credentials::default();
    assert!(resolve_openai_auth(&credentials).is_none());
}

#[test]
fn openai_empty_oauth_is_skipped() {
    let mut vars = BTreeMap::new();
    vars.insert("OPENAI_OAUTH_TOKEN".to_owned(), "   ".to_owned());
    vars.insert("OPENAI_API_KEY".to_owned(), "fallback-key".to_owned());
    let credentials = Credentials::from_map(vars);
    let auth = resolve_openai_auth(&credentials).expect("should resolve");
    assert_eq!(auth, OpenAiAuth::ApiKey("fallback-key".to_owned()));
}

// ---------------------------------------------------------------------------
// OpenAiAuth::secret_values
// ---------------------------------------------------------------------------

#[test]
fn openai_oauth_secret_values() {
    let auth = OpenAiAuth::OAuthToken("oauth-tok".to_owned());
    assert_eq!(auth.secret_values(), vec!["oauth-tok"]);
}

#[test]
fn openai_api_key_secret_values() {
    let auth = OpenAiAuth::ApiKey("api-key".to_owned());
    assert_eq!(auth.secret_values(), vec!["api-key"]);
}
