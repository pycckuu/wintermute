//! Tests for `src/executor/egress.rs` — egress proxy configuration generation.

use wintermute::executor::egress::generate_squid_config;

// ---------------------------------------------------------------------------
// Config generation tests
// ---------------------------------------------------------------------------

#[test]
fn generate_squid_config_includes_always_allowed_registries() {
    let config = generate_squid_config(&[]);

    // Package registries must always be present
    assert!(
        config.contains("pypi.org"),
        "pypi.org should always be allowed"
    );
    assert!(
        config.contains("files.pythonhosted.org"),
        "files.pythonhosted.org should always be allowed"
    );
    assert!(
        config.contains("registry.npmjs.org"),
        "registry.npmjs.org should always be allowed"
    );
    assert!(
        config.contains("crates.io"),
        "crates.io should always be allowed"
    );
    assert!(
        config.contains("static.crates.io"),
        "static.crates.io should always be allowed"
    );
}

#[test]
fn generate_squid_config_includes_user_domains() {
    let domains = vec!["github.com".to_owned(), "api.example.com".to_owned()];
    let config = generate_squid_config(&domains);

    assert!(config.contains(".github.com"));
    assert!(config.contains(".api.example.com"));
}

#[test]
fn generate_squid_config_denies_all_by_default() {
    let config = generate_squid_config(&[]);
    assert!(config.contains("http_access deny all"));
}

#[test]
fn generate_squid_config_allows_connect_for_https() {
    let config = generate_squid_config(&[]);
    assert!(config.contains("acl CONNECT method CONNECT"));
    assert!(config.contains("http_access allow CONNECT wintermute_allowed SSL_ports"));
}

#[test]
fn generate_squid_config_skips_empty_domain_strings() {
    let domains = vec!["valid.com".to_owned(), "".to_owned(), "  ".to_owned()];
    let config = generate_squid_config(&domains);

    assert!(config.contains(".valid.com"));
    // Empty strings should not create acl entries
    let user_section_start = config
        .find("# User-configured allowed domains")
        .expect("should have user section");
    let deny_section = config
        .find("# Allow CONNECT")
        .expect("should have CONNECT section");
    let user_section = &config[user_section_start..deny_section];

    // Only one user domain line (valid.com)
    let user_domain_lines: Vec<&str> = user_section
        .lines()
        .filter(|l| l.starts_with("acl wintermute_allowed"))
        .collect();
    assert_eq!(user_domain_lines.len(), 1);
}

#[test]
fn generate_squid_config_listens_on_port_3128() {
    let config = generate_squid_config(&[]);
    assert!(config.contains("http_port 3128"));
}

#[test]
fn generate_squid_config_disables_caching() {
    let config = generate_squid_config(&[]);
    assert!(config.contains("cache deny all"));
}

#[test]
fn generate_squid_config_logs_to_stdout() {
    let config = generate_squid_config(&[]);
    assert!(config.contains("access_log stdio:/dev/stdout"));
}
