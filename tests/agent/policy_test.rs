//! Policy gate and SSRF filter tests.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use wintermute::agent::policy::{
    check_policy, is_private_ip, PolicyContext, PolicyDecision, RateLimiter,
};
use wintermute::executor::ExecutorKind;

fn default_ctx(kind: ExecutorKind) -> PolicyContext {
    PolicyContext {
        allowed_domains: vec!["api.example.com".to_owned()],
        blocked_domains: vec![],
        always_approve_domains: vec![],
        executor_kind: kind,
    }
}

fn always_false(_domain: &str) -> bool {
    false
}

fn always_true(_domain: &str) -> bool {
    true
}

// ---------- is_private_ip tests ----------

#[test]
fn private_ip_loopback_v4() {
    assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
}

#[test]
fn private_ip_ten_range() {
    assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
}

#[test]
fn private_ip_172_16_range() {
    assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
}

#[test]
fn private_ip_192_168_range() {
    assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1))));
}

#[test]
fn private_ip_link_local() {
    assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(169, 254, 0, 1))));
}

#[test]
fn private_ip_cgn() {
    assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
}

#[test]
fn private_ip_v6_loopback() {
    assert!(is_private_ip(&IpAddr::V6(Ipv6Addr::LOCALHOST)));
}

#[test]
fn private_ip_v6_unique_local() {
    // fc00::1
    assert!(is_private_ip(&IpAddr::V6(Ipv6Addr::new(
        0xfc00, 0, 0, 0, 0, 0, 0, 1
    ))));
}

#[test]
fn private_ip_v6_link_local() {
    // fe80::1
    assert!(is_private_ip(&IpAddr::V6(Ipv6Addr::new(
        0xfe80, 0, 0, 0, 0, 0, 0, 1
    ))));
}

#[test]
fn private_ip_v4_mapped_v6() {
    // ::ffff:127.0.0.1
    let addr: IpAddr = "::ffff:127.0.0.1".parse().expect("valid mapped addr");
    assert!(is_private_ip(&addr));
}

#[test]
fn public_ip_8_8_8_8() {
    assert!(!is_private_ip(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
}

#[test]
fn public_ip_1_1_1_1() {
    assert!(!is_private_ip(&IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
}

// ---------- check_policy tests ----------

#[test]
fn policy_allows_execute_command_for_docker() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"command": "ls -la"});
    let result = check_policy("execute_command", &input, &ctx, &always_false);
    assert_eq!(result, PolicyDecision::Allow);
}

#[test]
fn policy_allows_safe_tools() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({});

    for tool in &[
        "memory_search",
        "memory_save",
        "send_telegram",
        "create_tool",
    ] {
        let result = check_policy(tool, &input, &ctx, &always_false);
        assert_eq!(result, PolicyDecision::Allow, "tool: {tool}");
    }
}

#[test]
fn policy_requires_approval_for_web_request_unknown_domain() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"url": "https://unknown.example.com/api"});
    let result = check_policy("web_request", &input, &ctx, &always_false);
    assert_eq!(result, PolicyDecision::RequireApproval);
}

#[test]
fn policy_allows_web_request_to_trusted_domain() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"url": "https://api.example.com/data"});
    let result = check_policy("web_request", &input, &ctx, &always_true);
    assert_eq!(result, PolicyDecision::Allow);
}

#[test]
fn policy_allows_browser_navigate_to_trusted_domain() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"action": "navigate", "url": "https://api.example.com/page"});
    let result = check_policy("browser", &input, &ctx, &always_false);
    assert_eq!(result, PolicyDecision::Allow);
}

#[test]
fn policy_allows_browser_navigate_to_ledger_trusted_domain() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"action": "navigate", "url": "https://ledger.example.com/page"});
    let result = check_policy("browser", &input, &ctx, &always_true);
    assert_eq!(result, PolicyDecision::Allow);
}

#[test]
fn policy_requires_approval_for_browser_navigate_unknown_domain() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input =
        serde_json::json!({"action": "navigate", "url": "https://unknown.example.com/page"});
    let result = check_policy("browser", &input, &ctx, &always_false);
    assert_eq!(result, PolicyDecision::RequireApproval);
}

#[test]
fn policy_denies_browser_navigate_to_blocked_domain() {
    let ctx = PolicyContext {
        allowed_domains: vec!["api.example.com".to_owned()],
        blocked_domains: vec!["evil.example.com".to_owned()],
        always_approve_domains: vec![],
        executor_kind: ExecutorKind::Docker,
    };
    let input = serde_json::json!({"action": "navigate", "url": "https://evil.example.com/page"});
    let result = check_policy("browser", &input, &ctx, &always_false);
    assert!(matches!(result, PolicyDecision::Deny(_)));
}

#[test]
fn policy_denies_browser_navigate_invalid_url() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"action": "navigate", "url": "not-a-url"});
    let result = check_policy("browser", &input, &ctx, &always_false);
    assert!(matches!(result, PolicyDecision::Deny(_)));
}

#[test]
fn policy_denies_browser_navigate_url_without_host() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"action": "navigate", "url": "file:///tmp/local"});
    let result = check_policy("browser", &input, &ctx, &always_false);
    assert!(matches!(result, PolicyDecision::Deny(_)));
}

#[test]
fn policy_requires_approval_for_browser_navigate_always_approve_domain() {
    let ctx = PolicyContext {
        allowed_domains: vec!["api.example.com".to_owned()],
        blocked_domains: vec![],
        always_approve_domains: vec!["api.example.com".to_owned()],
        executor_kind: ExecutorKind::Docker,
    };
    let input = serde_json::json!({"action": "navigate", "url": "https://api.example.com/page"});
    let result = check_policy("browser", &input, &ctx, &always_false);
    assert!(matches!(result, PolicyDecision::RequireApproval));
}

#[test]
fn policy_allows_browser_non_navigate_actions() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"action": "click", "selector": "#submit"});
    let result = check_policy("browser", &input, &ctx, &always_false);
    assert_eq!(result, PolicyDecision::Allow);
}

#[test]
fn policy_requires_approval_for_browser_evaluate_action() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"action": "evaluate", "javascript": "document.title"});
    let result = check_policy("browser", &input, &ctx, &always_false);
    assert_eq!(result, PolicyDecision::RequireApproval);
}

#[test]
fn policy_allows_dynamic_tools() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({});
    // Dynamic tools execute inside the sandbox, so policy allows them.
    let result = check_policy("hack_planet", &input, &ctx, &always_false);
    assert!(matches!(result, PolicyDecision::Allow));
}

#[test]
fn policy_denies_dangerous_commands_in_direct_mode() {
    let ctx = default_ctx(ExecutorKind::Direct);

    let dangerous = vec![
        serde_json::json!({"command": "rm -rf /"}),
        serde_json::json!({"command": "sudo reboot"}),
    ];

    for input in &dangerous {
        let result = check_policy("execute_command", input, &ctx, &always_false);
        assert!(
            matches!(result, PolicyDecision::Deny(_)),
            "should deny: {input}"
        );
    }
}

// ---------- docker_manage policy tests ----------

#[test]
fn policy_requires_approval_for_docker_run() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"action": "run", "image": "postgres:16"});
    let result = check_policy("docker_manage", &input, &ctx, &always_false);
    assert_eq!(result, PolicyDecision::RequireApproval);
}

#[test]
fn policy_requires_approval_for_docker_pull() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"action": "pull", "image": "redis:latest"});
    let result = check_policy("docker_manage", &input, &ctx, &always_false);
    assert_eq!(result, PolicyDecision::RequireApproval);
}

#[test]
fn policy_allows_docker_ps() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"action": "ps"});
    let result = check_policy("docker_manage", &input, &ctx, &always_false);
    assert_eq!(result, PolicyDecision::Allow);
}

#[test]
fn policy_allows_docker_logs() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"action": "logs", "container": "my-pg"});
    let result = check_policy("docker_manage", &input, &ctx, &always_false);
    assert_eq!(result, PolicyDecision::Allow);
}

#[test]
fn policy_allows_docker_stop() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"action": "stop", "container": "my-pg"});
    let result = check_policy("docker_manage", &input, &ctx, &always_false);
    assert_eq!(result, PolicyDecision::Allow);
}

#[test]
fn policy_allows_docker_exec() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"action": "exec", "container": "my-pg", "args": {"command": "psql -c 'SELECT 1'"}});
    let result = check_policy("docker_manage", &input, &ctx, &always_false);
    assert_eq!(result, PolicyDecision::Allow);
}

#[test]
fn policy_allows_docker_inspect() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"action": "inspect", "container": "my-pg"});
    let result = check_policy("docker_manage", &input, &ctx, &always_false);
    assert_eq!(result, PolicyDecision::Allow);
}

// ---------- rate limiter tests ----------

#[test]
fn rate_limiter_allows_under_limit() {
    let limiter = RateLimiter::new(60, 3);
    limiter.record();
    limiter.record();
    assert!(limiter.check("test").is_ok());
}

#[test]
fn rate_limiter_blocks_at_limit() {
    let limiter = RateLimiter::new(60, 3);
    limiter.record();
    limiter.record();
    limiter.record();
    let result = limiter.check("test");
    assert!(result.is_err());
}

#[test]
fn policy_denies_blocked_domain_for_web_request() {
    let ctx = PolicyContext {
        allowed_domains: vec!["api.example.com".to_owned()],
        blocked_domains: vec!["evil.example.com".to_owned()],
        always_approve_domains: vec![],
        executor_kind: ExecutorKind::Docker,
    };
    let input = serde_json::json!({"url": "https://evil.example.com/data", "method": "POST"});
    let result = check_policy("web_request", &input, &ctx, &always_false);
    assert!(
        matches!(result, PolicyDecision::Deny(_)),
        "should deny blocked domain"
    );
}

#[test]
fn policy_requires_approval_for_always_approve_domain() {
    let ctx = PolicyContext {
        allowed_domains: vec!["api.example.com".to_owned()],
        blocked_domains: vec![],
        always_approve_domains: vec!["api.example.com".to_owned()],
        executor_kind: ExecutorKind::Docker,
    };
    let input = serde_json::json!({"url": "https://api.example.com/action", "method": "POST"});
    let result = check_policy("web_request", &input, &ctx, &always_false);
    assert!(
        matches!(result, PolicyDecision::RequireApproval),
        "should require approval for always_approve domain"
    );
}

#[test]
fn policy_allows_web_fetch_regardless_of_domain() {
    let ctx = default_ctx(ExecutorKind::Docker);
    let input = serde_json::json!({"url": "https://unknown-site.com"});
    let result = check_policy("web_fetch", &input, &ctx, &always_false);
    assert!(matches!(result, PolicyDecision::Allow));
}
