//! Tests for OAuth token expiry detection, refresh error paths, and .env persistence.

use std::io::Write;

use wintermute::credentials::{
    is_token_expired, refresh_anthropic_token, update_env_credentials, AnthropicAuth,
};

// ---------------------------------------------------------------------------
// is_token_expired
// ---------------------------------------------------------------------------

#[test]
fn expired_for_past_timestamp() {
    let auth = AnthropicAuth::OAuth {
        access_token: "tok".to_owned(),
        refresh_token: None,
        expires_at: Some(1_000_000), // way in the past
    };
    assert!(is_token_expired(&auth));
}

#[test]
fn not_expired_for_future_timestamp() {
    let auth = AnthropicAuth::OAuth {
        access_token: "tok".to_owned(),
        refresh_token: None,
        expires_at: Some(i64::MAX), // far future
    };
    assert!(!is_token_expired(&auth));
}

#[test]
fn expired_within_60s_buffer() {
    let now_ms = chrono::Utc::now().timestamp_millis();
    // Expires 30 seconds from now — within the 60s buffer.
    let auth = AnthropicAuth::OAuth {
        access_token: "tok".to_owned(),
        refresh_token: None,
        expires_at: Some(now_ms.saturating_add(30_000)),
    };
    assert!(is_token_expired(&auth));
}

#[test]
fn not_expired_for_api_key() {
    let auth = AnthropicAuth::ApiKey("key".to_owned());
    assert!(!is_token_expired(&auth));
}

#[test]
fn not_expired_when_no_expiry() {
    let auth = AnthropicAuth::OAuth {
        access_token: "tok".to_owned(),
        refresh_token: None,
        expires_at: None,
    };
    assert!(!is_token_expired(&auth));
}

// ---------------------------------------------------------------------------
// refresh_anthropic_token — error paths (no network)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn refresh_returns_error_when_no_refresh_token() {
    let auth = AnthropicAuth::OAuth {
        access_token: "tok".to_owned(),
        refresh_token: None,
        expires_at: Some(1),
    };
    let err = refresh_anthropic_token(&auth)
        .await
        .expect_err("should fail");
    assert!(err.to_string().contains("no refresh token"));
}

#[tokio::test]
async fn refresh_returns_error_for_api_key() {
    let auth = AnthropicAuth::ApiKey("key".to_owned());
    let err = refresh_anthropic_token(&auth)
        .await
        .expect_err("should fail");
    assert!(err.to_string().contains("no refresh token"));
}

#[tokio::test]
async fn refresh_returns_error_for_empty_refresh_token() {
    let auth = AnthropicAuth::OAuth {
        access_token: "tok".to_owned(),
        refresh_token: Some(String::new()),
        expires_at: Some(1),
    };
    let err = refresh_anthropic_token(&auth)
        .await
        .expect_err("should fail");
    assert!(err.to_string().contains("no refresh token"));
}

// ---------------------------------------------------------------------------
// update_env_credentials — .env round-trip
// ---------------------------------------------------------------------------

#[test]
fn update_env_writes_tokens() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env_path = dir.path().join(".env");

    // Write an initial .env with commented-out OAuth lines.
    {
        let mut f = std::fs::File::create(&env_path).expect("create");
        writeln!(f, "WINTERMUTE_TELEGRAM_TOKEN=bot-tok").expect("write");
        writeln!(f, "ANTHROPIC_API_KEY=").expect("write");
        writeln!(f, "# ANTHROPIC_OAUTH_TOKEN=").expect("write");
        writeln!(f, "# ANTHROPIC_OAUTH_REFRESH_TOKEN=").expect("write");
        writeln!(f, "# ANTHROPIC_OAUTH_EXPIRES_AT=").expect("write");
    }

    // Set permissions to 0600 so validation passes.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&env_path, std::fs::Permissions::from_mode(0o600))
            .expect("set perms");
    }

    let auth = AnthropicAuth::OAuth {
        access_token: "new-access".to_owned(),
        refresh_token: Some("new-refresh".to_owned()),
        expires_at: Some(9999999999999),
    };
    update_env_credentials(&env_path, &auth).expect("update should succeed");

    let content = std::fs::read_to_string(&env_path).expect("read");
    assert!(
        content.contains("ANTHROPIC_OAUTH_TOKEN=new-access"),
        "access token not found in:\n{content}"
    );
    assert!(
        content.contains("ANTHROPIC_OAUTH_REFRESH_TOKEN=new-refresh"),
        "refresh token not found in:\n{content}"
    );
    assert!(
        content.contains("ANTHROPIC_OAUTH_EXPIRES_AT=9999999999999"),
        "expires_at not found in:\n{content}"
    );
    // Ensure existing lines are preserved.
    assert!(
        content.contains("WINTERMUTE_TELEGRAM_TOKEN=bot-tok"),
        "telegram token lost in:\n{content}"
    );
}

#[test]
fn update_env_is_noop_for_api_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env_path = dir.path().join(".env");
    std::fs::write(&env_path, "ANTHROPIC_API_KEY=my-key\n").expect("write");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&env_path, std::fs::Permissions::from_mode(0o600))
            .expect("set perms");
    }

    let auth = AnthropicAuth::ApiKey("my-key".to_owned());
    update_env_credentials(&env_path, &auth).expect("should be no-op");

    let content = std::fs::read_to_string(&env_path).expect("read");
    assert_eq!(content, "ANTHROPIC_API_KEY=my-key\n");
}
