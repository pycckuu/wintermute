//! Policy gate for tool execution, SSRF filtering, and rate limiting.
//!
//! The policy module decides whether a tool call should be allowed, require
//! user approval, or be denied outright. It also provides SSRF protection
//! by resolving DNS and checking for private IP addresses, and per-tool
//! rate limiting to prevent abuse.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

use url::Url;

use crate::executor::ExecutorKind;

// ---------------------------------------------------------------------------
// Policy decision and errors
// ---------------------------------------------------------------------------

/// Outcome of a policy check for a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Tool call is permitted.
    Allow,
    /// Tool call requires explicit user approval before proceeding.
    RequireApproval,
    /// Tool call is denied with a reason.
    Deny(String),
}

/// Errors from policy enforcement.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// The target URL resolved to a private/internal IP address.
    #[error("SSRF blocked for {url}: {reason}")]
    SsrfBlocked {
        /// The offending URL.
        url: String,
        /// Why the request was blocked.
        reason: String,
    },

    /// Tool call rate limit exceeded.
    #[error("rate limited for tool {tool}: {detail}")]
    RateLimited {
        /// The tool that was rate-limited.
        tool: String,
        /// Details about the limit.
        detail: String,
    },

    /// Action is forbidden by policy.
    #[error("forbidden: {0}")]
    Forbidden(String),
}

// ---------------------------------------------------------------------------
// Policy context
// ---------------------------------------------------------------------------

/// Configuration context used by the policy gate.
#[derive(Debug, Clone)]
pub struct PolicyContext {
    /// Domains pre-approved for outbound requests.
    pub allowed_domains: Vec<String>,
    /// Domains blocked entirely.
    pub blocked_domains: Vec<String>,
    /// Domains that always require explicit approval.
    pub always_approve_domains: Vec<String>,
    /// Current executor implementation kind.
    pub executor_kind: ExecutorKind,
}

// ---------------------------------------------------------------------------
// Policy check
// ---------------------------------------------------------------------------

/// Dangerous command prefixes that are denied in Direct executor mode.
const DANGEROUS_COMMANDS: &[&str] = &[
    "rm -rf /",
    "rm -rf ~",
    "sudo ",
    "mkfs",
    "dd if=",
    ":(){",
    "chmod -R 777 /",
    "shutdown",
    "reboot",
    "halt",
    "init 0",
    "init 6",
];

/// Evaluate the policy for a given tool call.
///
/// Returns [`PolicyDecision::Allow`], [`PolicyDecision::RequireApproval`],
/// or [`PolicyDecision::Deny`] depending on the tool, its input, and the
/// current executor configuration.
pub fn check_policy(
    tool_name: &str,
    input: &serde_json::Value,
    ctx: &PolicyContext,
    is_domain_trusted: &dyn Fn(&str) -> bool,
) -> PolicyDecision {
    match tool_name {
        "execute_command" => check_execute_command(input, ctx),
        "web_fetch" => PolicyDecision::Allow,
        "web_request" => check_domain_policy(input, ctx, is_domain_trusted),
        "browser" => check_browser_policy(input, ctx, is_domain_trusted),
        "docker_manage" => check_docker_manage(input),
        "memory_search" | "memory_save" | "send_message" | "create_tool" | "manage_brief"
        | "read_messages" => PolicyDecision::Allow,
        // Dynamic tools execute inside the sandbox via the executor, so they are allowed.
        _ => PolicyDecision::Allow,
    }
}

/// Check execute_command: allow if Docker, restrict dangerous commands if Direct.
fn check_execute_command(input: &serde_json::Value, ctx: &PolicyContext) -> PolicyDecision {
    match ctx.executor_kind {
        ExecutorKind::Docker => PolicyDecision::Allow,
        ExecutorKind::Direct => {
            let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            for prefix in DANGEROUS_COMMANDS {
                if command.contains(prefix) {
                    return PolicyDecision::Deny(format!(
                        "dangerous command blocked in Direct mode: {command}"
                    ));
                }
            }
            PolicyDecision::Allow
        }
    }
}

/// Extract domain from a URL field in tool input and check domain policy.
fn check_domain_policy(
    input: &serde_json::Value,
    ctx: &PolicyContext,
    is_domain_trusted: &dyn Fn(&str) -> bool,
) -> PolicyDecision {
    let url_str = input.get("url").and_then(|v| v.as_str()).unwrap_or("");

    let domain = match Url::parse(url_str) {
        Ok(parsed) => parsed.host_str().unwrap_or("").to_owned(),
        Err(_) => return PolicyDecision::Deny(format!("invalid URL: {url_str}")),
    };

    if domain.is_empty() {
        return PolicyDecision::Deny("URL has no host".to_owned());
    }

    evaluate_domain(&domain, ctx, is_domain_trusted)
}

/// Check browser policy: navigate actions check domain, other actions are allowed.
fn check_browser_policy(
    input: &serde_json::Value,
    ctx: &PolicyContext,
    is_domain_trusted: &dyn Fn(&str) -> bool,
) -> PolicyDecision {
    let action = input.get("action").and_then(|v| v.as_str()).unwrap_or("");

    if action == "navigate" {
        return check_domain_policy(input, ctx, is_domain_trusted);
    }
    if action == "evaluate" {
        return PolicyDecision::RequireApproval;
    }
    PolicyDecision::Allow
}

/// Check docker_manage: pull/run require approval, other actions are allowed.
fn check_docker_manage(input: &serde_json::Value) -> PolicyDecision {
    let action = input.get("action").and_then(|v| v.as_str()).unwrap_or("");
    match action {
        "pull" | "run" => PolicyDecision::RequireApproval,
        _ => PolicyDecision::Allow,
    }
}

/// Core domain evaluation logic shared by web_request and browser navigate.
fn evaluate_domain(
    domain: &str,
    ctx: &PolicyContext,
    is_domain_trusted: &dyn Fn(&str) -> bool,
) -> PolicyDecision {
    // Blocked domains are always denied.
    if ctx.blocked_domains.iter().any(|d| d == domain) {
        return PolicyDecision::Deny(format!("domain is blocked: {domain}"));
    }

    // Always-approve domains require approval even if otherwise trusted.
    if ctx.always_approve_domains.iter().any(|d| d == domain) {
        return PolicyDecision::RequireApproval;
    }

    // Allowed in config or trusted in trust ledger — allow.
    if ctx.allowed_domains.iter().any(|d| d == domain) || is_domain_trusted(domain) {
        return PolicyDecision::Allow;
    }

    PolicyDecision::RequireApproval
}

// ---------------------------------------------------------------------------
// SSRF protection
// ---------------------------------------------------------------------------

/// Check whether an IP address is in a private/reserved range.
pub fn is_private_ip(addr: &IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // 127.0.0.0/8
            octets[0] == 127
            // 10.0.0.0/8
            || octets[0] == 10
            // 172.16.0.0/12
            || (octets[0] == 172 && (octets[1] & 0xF0) == 16)
            // 192.168.0.0/16
            || (octets[0] == 192 && octets[1] == 168)
            // 169.254.0.0/16 (link-local)
            || (octets[0] == 169 && octets[1] == 254)
            // 100.64.0.0/10 (CGN)
            || (octets[0] == 100 && (octets[1] & 0xC0) == 64)
            // 0.0.0.0
            || (octets[0] == 0 && octets[1] == 0 && octets[2] == 0 && octets[3] == 0)
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            // ::1
            v6.is_loopback()
            // fc00::/7
            || (segments[0] & 0xFE00) == 0xFC00
            // fe80::/10
            || (segments[0] & 0xFFC0) == 0xFE80
            // ::ffff:0:0/96 — IPv4-mapped addresses
            || check_v4_mapped(v6)
        }
    }
}

/// Check IPv4-mapped IPv6 addresses (::ffff:x.x.x.x).
fn check_v4_mapped(v6: &std::net::Ipv6Addr) -> bool {
    let segments = v6.segments();
    // IPv4-mapped: first 5 segments are 0, segment[5] is 0xFFFF
    if segments[0] == 0
        && segments[1] == 0
        && segments[2] == 0
        && segments[3] == 0
        && segments[4] == 0
        && segments[5] == 0xFFFF
    {
        // Extract embedded IPv4 from last two segments.
        // Each segment is u16; mask to u8 for the low byte.
        let hi = segments[6];
        let lo = segments[7];
        let a = (hi >> 8) & 0xFF;
        let b = hi & 0xFF;
        let c = (lo >> 8) & 0xFF;
        let d = lo & 0xFF;
        #[allow(clippy::cast_possible_truncation)]
        let v4 = std::net::Ipv4Addr::new(a as u8, b as u8, c as u8, d as u8);
        return is_private_ip(&IpAddr::V4(v4));
    }
    false
}

/// Resolve a URL's host via DNS and verify no resolved address is private.
///
/// # Errors
///
/// Returns [`PolicyError::SsrfBlocked`] if any resolved IP is in a private range.
pub async fn ssrf_check(url: &Url) -> Result<(), PolicyError> {
    let host = url.host_str().ok_or_else(|| PolicyError::SsrfBlocked {
        url: url.to_string(),
        reason: "URL has no host".to_owned(),
    })?;

    let port = url.port_or_known_default().unwrap_or(80);
    let addr_str = format!("{host}:{port}");

    let addrs = tokio::net::lookup_host(&addr_str)
        .await
        .map_err(|e| PolicyError::SsrfBlocked {
            url: url.to_string(),
            reason: format!("DNS resolution failed: {e}"),
        })?;

    for addr in addrs {
        if is_private_ip(&addr.ip()) {
            return Err(PolicyError::SsrfBlocked {
                url: url.to_string(),
                reason: format!("resolved to private IP: {}", addr.ip()),
            });
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Rate limiter
// ---------------------------------------------------------------------------

/// Sliding-window rate limiter for tool calls.
///
/// Uses a sync [`Mutex`] since the critical section is very short (no awaits).
#[derive(Debug)]
pub struct RateLimiter {
    window: Mutex<VecDeque<Instant>>,
    max_count: u32,
    window_secs: u64,
}

impl RateLimiter {
    /// Create a new rate limiter with the given window size and maximum count.
    pub fn new(window_secs: u64, max_count: u32) -> Self {
        Self {
            window: Mutex::new(VecDeque::new()),
            max_count,
            window_secs,
        }
    }

    /// Check whether the rate limit allows another call.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError::RateLimited`] when the limit is exceeded.
    pub fn check(&self, tool_name: &str) -> Result<(), PolicyError> {
        let mut window = self
            .window
            .lock()
            .map_err(|e| PolicyError::Forbidden(format!("rate limiter lock poisoned: {e}")))?;

        let cutoff = Instant::now()
            .checked_sub(std::time::Duration::from_secs(self.window_secs))
            .unwrap_or_else(Instant::now);

        // Drain expired entries
        while window.front().is_some_and(|t| *t < cutoff) {
            window.pop_front();
        }

        let count = u32::try_from(window.len()).unwrap_or(u32::MAX);
        if count >= self.max_count {
            return Err(PolicyError::RateLimited {
                tool: tool_name.to_owned(),
                detail: format!(
                    "{count} calls in the last {window_secs}s (limit: {max})",
                    window_secs = self.window_secs,
                    max = self.max_count,
                ),
            });
        }
        Ok(())
    }

    /// Record that a tool call has been made.
    pub fn record(&self) {
        if let Ok(mut window) = self.window.lock() {
            window.push_back(Instant::now());
        }
    }
}
