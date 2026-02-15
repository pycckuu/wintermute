//! Skill discovery and configuration (feature-self-extending-skills, spec 3, 14).
//!
//! A skill is a directory in `~/.pfar/skills/` containing a `skill.toml`
//! manifest and server code. Skills are MCP servers that PFAR generated
//! (or the owner placed manually). At startup, [`find_local_skills`] scans
//! the skills directory, parses each manifest, and converts to
//! [`McpServerConfig`](super::McpServerConfig) for spawning via the
//! existing MCP infrastructure.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{McpServerCommand, McpServerConfig};

// ── Skill config types (feature-self-extending-skills, spec 3) ──

/// Skill manifest loaded from `skill.toml` (feature-self-extending-skills, spec 3).
///
/// Superset of [`McpServerConfig`] with metadata, sandbox, and search fields.
/// Converts to `McpServerConfig` via [`to_mcp_config`](Self::to_mcp_config).
#[derive(Debug, Clone, Deserialize)]
pub struct SkillConfig {
    /// Skill name (e.g., "uptime-checker").
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Version string (monotonic integer as string).
    #[serde(default = "default_version")]
    pub version: String,
    /// When the skill was created (RFC 3339).
    #[serde(default)]
    pub created_at: Option<String>,
    /// Who created it: "agent" or "owner" (feature-self-extending-skills, spec 3).
    #[serde(default = "default_created_by")]
    pub created_by: String,
    /// Security label for outputs (e.g., "public", "internal").
    pub label: String,
    /// Domain allowlist for network access (feature-self-extending-skills, spec 3).
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Server command configuration.
    pub server: SkillServerCommand,
    /// Credential env vars mapped to vault references.
    #[serde(default)]
    pub auth: HashMap<String, String>,
    /// Sandbox constraints (informational; enforcement added with bubblewrap).
    #[serde(default)]
    pub sandbox: Option<SkillSandboxConfig>,
    /// Search metadata for skill retrieval (feature-self-extending-skills, spec 9).
    #[serde(default)]
    pub search: Option<SkillSearchConfig>,
}

/// Server command for a skill (feature-self-extending-skills, spec 3).
#[derive(Debug, Clone, Deserialize)]
pub struct SkillServerCommand {
    /// Executable (e.g., "python3").
    pub command: String,
    /// Arguments (e.g., \["server.py"\]).
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory override. If omitted, uses the skill directory.
    pub working_dir: Option<String>,
}

/// Sandbox configuration for a skill (feature-self-extending-skills, spec 3).
///
/// Informational in the current phase; enforcement added with
/// bubblewrap integration.
#[derive(Debug, Clone, Deserialize)]
pub struct SkillSandboxConfig {
    /// Memory limit (e.g., "128m").
    #[serde(default)]
    pub memory_limit: Option<String>,
    /// Whether the filesystem is read-only.
    #[serde(default)]
    pub read_only_fs: Option<bool>,
    /// Whether /tmp access is allowed.
    #[serde(default)]
    pub allow_tmp: Option<bool>,
}

/// Search metadata for semantic skill retrieval (feature-self-extending-skills, spec 9).
#[derive(Debug, Clone, Deserialize)]
pub struct SkillSearchConfig {
    /// Keywords for matching user queries.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Example natural-language queries this skill handles.
    #[serde(default)]
    pub example_queries: Vec<String>,
}

fn default_version() -> String {
    "1".to_owned()
}

fn default_created_by() -> String {
    "owner".to_owned()
}

impl SkillConfig {
    /// Convert to [`McpServerConfig`] for spawning via `McpServerManager`
    /// (feature-self-extending-skills, spec 2, 8).
    ///
    /// The `skill_dir` is used to resolve relative file paths in args.
    /// If `server.working_dir` is set, it takes precedence over the
    /// skill directory.
    pub fn to_mcp_config(&self, skill_dir: &Path) -> McpServerConfig {
        // Resolve working directory: explicit override or skill directory.
        let working_dir = self
            .server
            .working_dir
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| skill_dir.to_path_buf());

        // Resolve relative file paths in args to absolute paths.
        // This allows skill.toml to reference "server.py" which gets
        // resolved to the full path inside the skill directory.
        let resolved_args: Vec<String> = self
            .server
            .args
            .iter()
            .map(|arg| {
                let p = Path::new(arg);
                if p.is_relative() && !arg.starts_with('-') {
                    working_dir.join(p).to_string_lossy().into_owned()
                } else {
                    arg.clone()
                }
            })
            .collect();

        McpServerConfig {
            name: self.name.clone(),
            description: self.description.clone(),
            label: self.label.clone(),
            allowed_domains: self.allowed_domains.clone(),
            server: McpServerCommand {
                command: self.server.command.clone(),
                args: resolved_args,
            },
            auth: self.auth.clone(),
        }
    }
}

// ── Skill discovery (feature-self-extending-skills, spec 14) ──

/// Expand `~` prefix to the user's home directory
/// (feature-self-extending-skills).
///
/// Returns the path unchanged if it doesn't start with `~/`.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// Scan a directory for skill manifests and return parsed configs
/// (feature-self-extending-skills, spec 14).
///
/// Each subdirectory of `skills_dir` that contains a `skill.toml`
/// file is treated as a skill. Directories without `skill.toml` or
/// with parse errors are logged and skipped (non-fatal).
///
/// Returns a list of `(SkillConfig, PathBuf)` pairs where the `PathBuf`
/// is the absolute path to the skill directory.
pub fn find_local_skills(skills_dir: &str) -> Vec<(SkillConfig, PathBuf)> {
    let dir = expand_tilde(skills_dir);

    if !dir.is_dir() {
        tracing::debug!(path = %dir.display(), "skills directory does not exist, skipping");
        return Vec::new();
    }

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(path = %dir.display(), error = %e, "failed to read skills directory");
            return Vec::new();
        }
    };

    let mut skills = Vec::new();

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "failed to read skills directory entry");
                continue;
            }
        };

        let skill_dir = entry.path();
        if !skill_dir.is_dir() {
            continue;
        }

        let manifest_path = skill_dir.join("skill.toml");
        if !manifest_path.is_file() {
            continue;
        }

        match load_skill_config(&manifest_path) {
            Ok(config) => {
                tracing::info!(
                    skill = %config.name,
                    path = %skill_dir.display(),
                    "found local skill"
                );
                skills.push((config, skill_dir));
            }
            Err(e) => {
                tracing::warn!(
                    path = %manifest_path.display(),
                    error = %e,
                    "failed to parse skill manifest, skipping"
                );
            }
        }
    }

    skills
}

/// Load and parse a single `skill.toml` manifest
/// (feature-self-extending-skills, spec 3).
fn load_skill_config(path: &Path) -> Result<SkillConfig> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let config: SkillConfig =
        toml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_skill_toml() -> &'static str {
        r#"
name = "uptime-checker"
description = "Check HTTP endpoint uptime, response time, and SSL certificate expiry"
version = "1"
created_at = "2026-02-15T12:00:00Z"
created_by = "agent"
label = "public"
allowed_domains = ["status.myapp.io", "api.myapp.io"]

[server]
command = "python3"
args = ["server.py"]
working_dir = "~/.pfar/skills/uptime-checker"

[sandbox]
memory_limit = "128m"
read_only_fs = true
allow_tmp = true

[search]
keywords = ["uptime", "status", "health check", "ping"]
example_queries = [
    "is my server up?",
    "check uptime for status.myapp.io",
]
"#
    }

    #[test]
    fn test_skill_config_deserialize() {
        let config: SkillConfig =
            toml::from_str(sample_skill_toml()).expect("should parse full skill.toml");
        assert_eq!(config.name, "uptime-checker");
        assert_eq!(config.version, "1");
        assert_eq!(config.created_by, "agent");
        assert_eq!(config.label, "public");
        assert_eq!(config.allowed_domains.len(), 2);
        assert_eq!(config.server.command, "python3");
        assert_eq!(config.server.args, vec!["server.py"]);
        assert_eq!(
            config.server.working_dir.as_deref(),
            Some("~/.pfar/skills/uptime-checker")
        );

        let sandbox = config.sandbox.expect("sandbox should exist");
        assert_eq!(sandbox.memory_limit.as_deref(), Some("128m"));
        assert_eq!(sandbox.read_only_fs, Some(true));
        assert_eq!(sandbox.allow_tmp, Some(true));

        let search = config.search.expect("search should exist");
        assert_eq!(search.keywords.len(), 4);
        assert_eq!(search.example_queries.len(), 2);
    }

    #[test]
    fn test_skill_config_minimal() {
        let toml_str = r#"
name = "simple-tool"
description = "A simple computation tool"
label = "public"

[server]
command = "python3"
"#;
        let config: SkillConfig = toml::from_str(toml_str).expect("should parse minimal");
        assert_eq!(config.name, "simple-tool");
        assert_eq!(config.version, "1"); // default
        assert_eq!(config.created_by, "owner"); // default
        assert!(config.allowed_domains.is_empty());
        assert!(config.auth.is_empty());
        assert!(config.server.args.is_empty());
        assert!(config.sandbox.is_none());
        assert!(config.search.is_none());
    }

    #[test]
    fn test_to_mcp_config() {
        let config: SkillConfig = toml::from_str(sample_skill_toml()).expect("should parse");
        let skill_dir = PathBuf::from("/home/user/.pfar/skills/uptime-checker");
        let mcp = config.to_mcp_config(&skill_dir);

        assert_eq!(mcp.name, "uptime-checker");
        assert_eq!(mcp.description, config.description);
        assert_eq!(mcp.label, "public");
        assert_eq!(mcp.allowed_domains, vec!["status.myapp.io", "api.myapp.io"]);
        assert_eq!(mcp.server.command, "python3");
        // working_dir is set explicitly in the skill.toml, so args resolve
        // against that path.
        assert_eq!(
            mcp.server.args,
            vec!["~/.pfar/skills/uptime-checker/server.py"]
        );
    }

    #[test]
    fn test_to_mcp_config_resolves_relative_args() {
        let toml_str = r#"
name = "test-skill"
description = "test"
label = "internal"

[server]
command = "python3"
args = ["server.py", "--port", "8080"]
"#;
        let config: SkillConfig = toml::from_str(toml_str).expect("should parse");
        let skill_dir = PathBuf::from("/skills/test-skill");
        let mcp = config.to_mcp_config(&skill_dir);

        // "server.py" is relative → resolved to /skills/test-skill/server.py
        // "--port" starts with '-' → kept as-is
        // "8080" is relative but not a file path, still resolved (acceptable)
        assert_eq!(mcp.server.args[0], "/skills/test-skill/server.py");
        assert_eq!(mcp.server.args[1], "--port");
        assert_eq!(mcp.server.args[2], "/skills/test-skill/8080");
    }

    #[test]
    fn test_to_mcp_config_with_auth() {
        let toml_str = r#"
name = "api-wrapper"
description = "Wraps an API"
label = "internal"

[server]
command = "python3"
args = ["server.py"]

[auth]
MY_API_KEY = "vault:myapp_api_key"
"#;
        let config: SkillConfig = toml::from_str(toml_str).expect("should parse");
        let skill_dir = PathBuf::from("/skills/api-wrapper");
        let mcp = config.to_mcp_config(&skill_dir);

        assert_eq!(
            mcp.auth.get("MY_API_KEY").map(|s| s.as_str()),
            Some("vault:myapp_api_key")
        );
    }

    #[test]
    fn test_find_local_skills_nonexistent_dir() {
        let skills = find_local_skills("/nonexistent/path/to/skills");
        assert!(skills.is_empty(), "nonexistent dir should return empty");
    }

    #[test]
    fn test_find_local_skills_empty_dir() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let skills = find_local_skills(tmp.path().to_str().expect("path"));
        assert!(skills.is_empty(), "empty dir should return empty");
    }

    #[test]
    fn test_find_local_skills_with_skills() {
        let tmp = tempfile::tempdir().expect("create tempdir");

        // Create a valid skill directory.
        let skill_dir = tmp.path().join("uptime-checker");
        std::fs::create_dir(&skill_dir).expect("mkdir");
        std::fs::write(
            skill_dir.join("skill.toml"),
            r#"
name = "uptime-checker"
description = "Check uptime"
label = "public"

[server]
command = "python3"
args = ["server.py"]
"#,
        )
        .expect("write");

        // Create a second valid skill.
        let skill_dir2 = tmp.path().join("weather");
        std::fs::create_dir(&skill_dir2).expect("mkdir");
        std::fs::write(
            skill_dir2.join("skill.toml"),
            r#"
name = "weather"
description = "Weather checker"
label = "internal"

[server]
command = "python3"
args = ["server.py"]
"#,
        )
        .expect("write");

        let skills = find_local_skills(tmp.path().to_str().expect("path"));
        assert_eq!(skills.len(), 2, "should find 2 skills");

        let names: Vec<&str> = skills.iter().map(|(c, _)| c.name.as_str()).collect();
        assert!(names.contains(&"uptime-checker"));
        assert!(names.contains(&"weather"));
    }

    #[test]
    fn test_find_local_skills_skips_invalid() {
        let tmp = tempfile::tempdir().expect("create tempdir");

        // Valid skill.
        let valid = tmp.path().join("valid-skill");
        std::fs::create_dir(&valid).expect("mkdir");
        std::fs::write(
            valid.join("skill.toml"),
            r#"
name = "valid"
description = "A valid skill"
label = "public"

[server]
command = "python3"
"#,
        )
        .expect("write");

        // Invalid skill (bad TOML).
        let invalid = tmp.path().join("broken-skill");
        std::fs::create_dir(&invalid).expect("mkdir");
        std::fs::write(invalid.join("skill.toml"), "this is {{ not valid toml").expect("write");

        // Directory without skill.toml (ignored).
        let no_manifest = tmp.path().join("no-manifest");
        std::fs::create_dir(&no_manifest).expect("mkdir");

        // Regular file (not a directory, ignored).
        std::fs::write(tmp.path().join("not-a-dir.txt"), "hello").expect("write");

        let skills = find_local_skills(tmp.path().to_str().expect("path"));
        assert_eq!(skills.len(), 1, "should find only the valid skill");
        assert_eq!(skills[0].0.name, "valid");
    }

    #[test]
    fn test_expand_tilde() {
        // With HOME set (normal case).
        if std::env::var_os("HOME").is_some() {
            let expanded = expand_tilde("~/foo/bar");
            assert!(
                !expanded.starts_with("~"),
                "tilde should be expanded: {expanded:?}"
            );
            assert!(
                expanded.to_string_lossy().ends_with("/foo/bar"),
                "path suffix preserved"
            );
        }

        // Non-tilde path returned unchanged.
        let unchanged = expand_tilde("/absolute/path");
        assert_eq!(unchanged, PathBuf::from("/absolute/path"));

        // Relative path returned unchanged.
        let relative = expand_tilde("relative/path");
        assert_eq!(relative, PathBuf::from("relative/path"));
    }
}
