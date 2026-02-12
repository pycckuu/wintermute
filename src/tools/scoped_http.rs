//! Domain-scoped HTTP client with private IP blocking (spec 5.4, 16.3).
//!
//! Tools receive a `ScopedHttpClient` instead of a raw `reqwest::Client`.
//! The client rejects requests to non-allowlisted domains and private IP
//! ranges (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 127.0.0.0/8),
//! enforcing network isolation per spec 16.3.

use std::collections::HashSet;
use std::net::IpAddr;

use thiserror::Error;

/// HTTP error for domain/IP policy violations (spec 5.4).
#[derive(Debug, Error)]
pub enum HttpError {
    /// Request to a domain not in the tool's allowlist.
    #[error("domain not in allowlist: {0}")]
    DomainNotAllowed(String),
    /// Request to a private/loopback IP address.
    #[error("private IP address blocked: {0}")]
    PrivateIpBlocked(String),
    /// Underlying HTTP transport error.
    #[error("HTTP request failed: {0}")]
    RequestFailed(#[from] reqwest::Error),
    /// URL could not be parsed.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
}

/// HTTP client scoped to a tool's domain allowlist (spec 5.4).
///
/// Rejects requests to non-allowlisted domains and private IP ranges.
/// Tools receive this instead of a raw `reqwest::Client` to enforce
/// network isolation (spec 16.3).
pub struct ScopedHttpClient {
    inner: reqwest::Client,
    allowed_domains: HashSet<String>,
}

impl ScopedHttpClient {
    /// Create a new scoped HTTP client restricted to the given domains (spec 5.4).
    pub fn new(allowed_domains: HashSet<String>) -> Self {
        Self {
            inner: reqwest::Client::new(),
            allowed_domains,
        }
    }

    /// Send a GET request to the given URL (spec 5.4).
    ///
    /// Validates the URL against the domain allowlist and private IP
    /// ranges before sending.
    pub async fn get(&self, url: &str) -> Result<reqwest::Response, HttpError> {
        self.validate_url(url)?;
        self.inner.get(url).send().await.map_err(HttpError::from)
    }

    /// Send a GET request with a Bearer authorization header (spec 5.4).
    ///
    /// Validates the URL against the domain allowlist and private IP
    /// ranges before sending. The token is sent as `Authorization: Bearer <token>`.
    pub async fn get_with_bearer(
        &self,
        url: &str,
        token: &str,
    ) -> Result<reqwest::Response, HttpError> {
        self.validate_url(url)?;
        self.inner
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(HttpError::from)
    }

    /// Send a POST request with a JSON body (spec 5.4).
    ///
    /// Validates the URL against the domain allowlist and private IP
    /// ranges before sending.
    pub async fn post(
        &self,
        url: &str,
        body: serde_json::Value,
    ) -> Result<reqwest::Response, HttpError> {
        self.validate_url(url)?;
        self.inner
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(HttpError::from)
    }

    /// Send a POST request with a JSON body and Bearer authorization header (spec 5.4).
    ///
    /// Validates the URL against the domain allowlist and private IP
    /// ranges before sending. The token is sent as `Authorization: Bearer <token>`.
    pub async fn post_with_bearer(
        &self,
        url: &str,
        body: serde_json::Value,
        token: &str,
    ) -> Result<reqwest::Response, HttpError> {
        self.validate_url(url)?;
        self.inner
            .post(url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(HttpError::from)
    }

    /// Validate a URL against the domain allowlist and private IP ranges (spec 5.4, 16.3).
    ///
    /// Steps:
    /// 1. Parse the URL and extract the host.
    /// 2. Check if the host is a private IP address (blocked).
    /// 3. Check if the host is in the allowed domains set.
    pub(crate) fn validate_url(&self, url: &str) -> Result<(), HttpError> {
        let parsed = reqwest::Url::parse(url).map_err(|e| HttpError::InvalidUrl(e.to_string()))?;

        let host = parsed
            .host_str()
            .ok_or_else(|| HttpError::InvalidUrl("URL has no host".to_owned()))?;

        // Step 1: block private IPs regardless of allowlist.
        if is_private_ip(host) {
            return Err(HttpError::PrivateIpBlocked(host.to_owned()));
        }

        // Step 2: check domain against allowlist.
        if !self.allowed_domains.contains(host) {
            return Err(HttpError::DomainNotAllowed(host.to_owned()));
        }

        Ok(())
    }
}

/// Check if a host string represents a private IP address (spec 16.3).
///
/// Blocks the following ranges:
/// - `10.0.0.0/8`
/// - `172.16.0.0/12`
/// - `192.168.0.0/16`
/// - `127.0.0.0/8` (loopback)
///
/// If the host is a hostname (not an IP literal), returns `false` —
/// DNS resolution happens later and hostnames are validated by the
/// domain allowlist instead.
fn is_private_ip(host: &str) -> bool {
    // Try to parse as IP address. If it fails, it's a hostname.
    let addr: IpAddr = match host.parse() {
        Ok(a) => a,
        Err(_) => return false,
    };

    match addr {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // 127.0.0.0/8 — loopback
            if octets[0] == 127 {
                return true;
            }
            // 10.0.0.0/8 — private
            if octets[0] == 10 {
                return true;
            }
            // 172.16.0.0/12 — private (172.16.x.x through 172.31.x.x)
            if octets[0] == 172 && (octets[1] >= 16 && octets[1] <= 31) {
                return true;
            }
            // 192.168.0.0/16 — private
            if octets[0] == 192 && octets[1] == 168 {
                return true;
            }
            false
        }
        IpAddr::V6(v6) => {
            // ::1 is loopback
            v6.is_loopback()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── validate_url tests (regression test 16) ──

    #[test]
    fn test_allowed_domain() {
        let mut domains = HashSet::new();
        domains.insert("api.github.com".to_owned());
        let client = ScopedHttpClient::new(domains);
        assert!(client.validate_url("https://api.github.com/repos").is_ok());
    }

    #[test]
    fn test_disallowed_domain() {
        let mut domains = HashSet::new();
        domains.insert("api.github.com".to_owned());
        let client = ScopedHttpClient::new(domains);
        let result = client.validate_url("https://evil.com/steal");
        assert!(matches!(result, Err(HttpError::DomainNotAllowed(d)) if d == "evil.com"));
    }

    #[test]
    fn test_private_ip_10_blocked() {
        let mut domains = HashSet::new();
        domains.insert("10.0.0.1".to_owned());
        let client = ScopedHttpClient::new(domains);
        let result = client.validate_url("http://10.0.0.1/internal");
        assert!(matches!(result, Err(HttpError::PrivateIpBlocked(ip)) if ip == "10.0.0.1"));
    }

    #[test]
    fn test_private_ip_172_blocked() {
        let mut domains = HashSet::new();
        domains.insert("172.16.0.1".to_owned());
        let client = ScopedHttpClient::new(domains);
        let result = client.validate_url("http://172.16.0.1/internal");
        assert!(matches!(result, Err(HttpError::PrivateIpBlocked(ip)) if ip == "172.16.0.1"));
    }

    #[test]
    fn test_private_ip_192_blocked() {
        let mut domains = HashSet::new();
        domains.insert("192.168.1.1".to_owned());
        let client = ScopedHttpClient::new(domains);
        let result = client.validate_url("http://192.168.1.1/router");
        assert!(matches!(result, Err(HttpError::PrivateIpBlocked(ip)) if ip == "192.168.1.1"));
    }

    #[test]
    fn test_private_ip_127_blocked() {
        let mut domains = HashSet::new();
        domains.insert("127.0.0.1".to_owned());
        let client = ScopedHttpClient::new(domains);
        let result = client.validate_url("http://127.0.0.1:8080/local");
        assert!(matches!(result, Err(HttpError::PrivateIpBlocked(ip)) if ip == "127.0.0.1"));
    }

    #[test]
    fn test_public_ip_allowed() {
        let mut domains = HashSet::new();
        domains.insert("8.8.8.8".to_owned());
        let client = ScopedHttpClient::new(domains);
        assert!(client.validate_url("http://8.8.8.8/dns").is_ok());
    }

    #[test]
    fn test_empty_allowlist_blocks_all() {
        let client = ScopedHttpClient::new(HashSet::new());
        let result = client.validate_url("https://api.github.com/repos");
        assert!(matches!(result, Err(HttpError::DomainNotAllowed(_))));
    }

    #[test]
    fn test_url_parsing_error() {
        let client = ScopedHttpClient::new(HashSet::new());
        let result = client.validate_url("not a url at all");
        assert!(matches!(result, Err(HttpError::InvalidUrl(_))));
    }

    // ── is_private_ip unit tests ──

    #[test]
    fn test_is_private_ip_hostname_returns_false() {
        assert!(!is_private_ip("api.github.com"));
    }

    #[test]
    fn test_is_private_ip_public_v4() {
        assert!(!is_private_ip("8.8.8.8"));
        assert!(!is_private_ip("1.1.1.1"));
    }

    #[test]
    fn test_is_private_ip_172_boundary() {
        // 172.15.x.x is NOT private (below 172.16.0.0/12)
        assert!(!is_private_ip("172.15.255.255"));
        // 172.16.0.0 IS private
        assert!(is_private_ip("172.16.0.0"));
        // 172.31.255.255 IS private (top of /12)
        assert!(is_private_ip("172.31.255.255"));
        // 172.32.0.0 is NOT private (above 172.16.0.0/12)
        assert!(!is_private_ip("172.32.0.0"));
    }

    #[test]
    fn test_is_private_ip_v6_loopback() {
        assert!(is_private_ip("::1"));
    }

    #[test]
    fn test_is_private_ip_v6_public() {
        assert!(!is_private_ip("2001:db8::1"));
    }
}
