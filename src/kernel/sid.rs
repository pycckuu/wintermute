//! System Identity Document -- dynamically assembled runtime context
//! for LLM prompts (pfar-system-identity-document.md).
//!
//! The SID tells the LLM who it is, what it can do, and what's connected.
//! Injected as the first section of every LLM system prompt -- Planner,
//! Synthesizer, and fast path.
//!
//! Assembled at startup and rebuilt on state changes (MCP connect/disconnect,
//! persona config). Never contains secrets -- only catalog metadata.

use std::collections::HashSet;

/// Summary of an active MCP integration for SID rendering
/// (pfar-system-identity-document.md).
#[derive(Debug, Clone)]
pub struct IntegrationSummary {
    /// Service name (e.g., "notion").
    pub name: String,
    /// Number of discovered tools.
    pub tool_count: usize,
}

/// Summary of a built-in tool for SID rendering
/// (pfar-system-identity-document.md).
#[derive(Debug, Clone)]
pub struct ToolSummary {
    /// Tool name (e.g., "email").
    pub name: String,
    /// Number of actions.
    pub action_count: usize,
}

/// System Identity Document -- dynamically assembled runtime context
/// (pfar-system-identity-document.md).
///
/// Contains only catalog metadata (persona, tool names, integration names).
/// Never contains secrets, credentials, or user data.
pub struct SystemIdentityDocument {
    /// Persona string (e.g., "Atlas. Owner: Igor. Style: concise.").
    pub persona: Option<String>,
    /// Active MCP integrations with tool counts.
    pub integrations: Vec<IntegrationSummary>,
    /// Built-in tools (email, calendar, etc.) with action counts.
    pub builtin_tools: Vec<ToolSummary>,
}

impl SystemIdentityDocument {
    /// Render the SID to a text block for prompt injection
    /// (pfar-system-identity-document.md, ~100-200 tokens).
    pub fn render(&self) -> String {
        let mut out = String::new();

        // Persona section.
        if let Some(ref persona) = self.persona {
            out.push_str(&format!("You are {persona}.\n"));
        }

        // Capabilities section.
        out.push_str("\nCAPABILITIES:\n");

        if !self.integrations.is_empty() {
            let items: Vec<String> = self
                .integrations
                .iter()
                .map(|i| format!("{} ({} tools)", i.name, i.tool_count))
                .collect();
            out.push_str(&format!("- Integrations: {}\n", items.join(", ")));
        }

        if !self.builtin_tools.is_empty() {
            let items: Vec<String> = self.builtin_tools.iter().map(|t| t.name.clone()).collect();
            out.push_str(&format!("- Built-in tools: {}\n", items.join(", ")));
        }

        if self.integrations.is_empty() && self.builtin_tools.is_empty() {
            out.push_str("- No tools configured yet\n");
        }

        // Rules section (always present).
        out.push_str("\nRULES:\n");
        out.push_str("- Never mention internal architecture (pipeline, kernel, phases, planner, synthesizer, extractor)\n");
        out.push_str("- You are a personal assistant, not a system component\n");
        out.push_str("- When you lack a tool for something, say so directly\n");
        out.push_str("- When the owner mentions a connected service by name, use it\n");

        out
    }
}

/// Build a SID from available data sources (pfar-system-identity-document.md).
///
/// Call at startup and after state changes (MCP connect/disconnect, persona change).
/// Classifies tools as MCP integrations vs built-in based on `mcp_server_names`.
/// Omits the `admin` tool from display (internal, not user-facing).
pub fn build_sid(
    persona: Option<String>,
    tool_summaries: &[(String, usize)],
    mcp_server_names: &[String],
) -> SystemIdentityDocument {
    let mcp_set: HashSet<&str> = mcp_server_names.iter().map(|s| s.as_str()).collect();

    let mut integrations = Vec::new();
    let mut builtin_tools = Vec::new();

    for (name, count) in tool_summaries {
        if mcp_set.contains(name.as_str()) {
            integrations.push(IntegrationSummary {
                name: name.clone(),
                tool_count: *count,
            });
        } else if name != "admin" {
            // Omit admin tool from SID -- internal, not user-facing.
            builtin_tools.push(ToolSummary {
                name: name.clone(),
                action_count: *count,
            });
        }
    }

    SystemIdentityDocument {
        persona,
        integrations,
        builtin_tools,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_with_persona_and_integrations() {
        let sid = SystemIdentityDocument {
            persona: Some("Atlas. Owner: Igor. Style: concise.".to_owned()),
            integrations: vec![IntegrationSummary {
                name: "notion".to_owned(),
                tool_count: 22,
            }],
            builtin_tools: vec![
                ToolSummary {
                    name: "email".to_owned(),
                    action_count: 3,
                },
                ToolSummary {
                    name: "calendar".to_owned(),
                    action_count: 3,
                },
            ],
        };

        let rendered = sid.render();
        assert!(rendered.contains("You are Atlas. Owner: Igor. Style: concise."));
        assert!(rendered.contains("Integrations: notion (22 tools)"));
        assert!(rendered.contains("Built-in tools: email, calendar"));
        assert!(rendered.contains("RULES:"));
        assert!(rendered.contains("Never mention internal architecture"));
    }

    #[test]
    fn test_render_empty_state() {
        let sid = SystemIdentityDocument {
            persona: None,
            integrations: vec![],
            builtin_tools: vec![],
        };

        let rendered = sid.render();
        assert!(!rendered.starts_with("You are"));
        assert!(rendered.contains("No tools configured yet"));
        assert!(rendered.contains("RULES:"));
    }

    #[test]
    fn test_render_builtin_tools_only() {
        let sid = SystemIdentityDocument {
            persona: Some("Atlas".to_owned()),
            integrations: vec![],
            builtin_tools: vec![ToolSummary {
                name: "email".to_owned(),
                action_count: 3,
            }],
        };

        let rendered = sid.render();
        assert!(rendered.contains("You are Atlas."));
        assert!(rendered.contains("Built-in tools: email"));
        assert!(!rendered.contains("Integrations:"));
        assert!(!rendered.contains("No tools configured yet"));
    }

    #[test]
    fn test_build_sid_separates_mcp_from_builtin() {
        let tool_summaries = vec![
            ("admin".to_owned(), 10),
            ("calendar".to_owned(), 3),
            ("email".to_owned(), 3),
            ("memory".to_owned(), 1),
            ("notion".to_owned(), 22),
        ];
        let mcp_servers = vec!["notion".to_owned()];

        let sid = build_sid(Some("Atlas".to_owned()), &tool_summaries, &mcp_servers);

        assert_eq!(sid.integrations.len(), 1);
        assert_eq!(sid.integrations[0].name, "notion");
        assert_eq!(sid.integrations[0].tool_count, 22);

        // admin should be excluded, email + calendar + memory are built-in.
        assert_eq!(sid.builtin_tools.len(), 3);
        let names: Vec<&str> = sid.builtin_tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"calendar"));
        assert!(names.contains(&"email"));
        assert!(names.contains(&"memory"));
        assert!(!names.contains(&"admin"));
    }

    #[test]
    fn test_render_token_budget() {
        // SID with 3 integrations + 3 built-in tools should stay under 500 chars
        // (~125 tokens at 4 chars/token).
        let sid = SystemIdentityDocument {
            persona: Some("Atlas. Owner: Igor. Style: concise, dry humor.".to_owned()),
            integrations: vec![
                IntegrationSummary {
                    name: "notion".to_owned(),
                    tool_count: 22,
                },
                IntegrationSummary {
                    name: "github".to_owned(),
                    tool_count: 51,
                },
                IntegrationSummary {
                    name: "fetch".to_owned(),
                    tool_count: 1,
                },
            ],
            builtin_tools: vec![
                ToolSummary {
                    name: "email".to_owned(),
                    action_count: 3,
                },
                ToolSummary {
                    name: "calendar".to_owned(),
                    action_count: 3,
                },
                ToolSummary {
                    name: "memory".to_owned(),
                    action_count: 1,
                },
            ],
        };

        let rendered = sid.render();
        assert!(
            rendered.len() < 500,
            "SID should be under 500 chars, got {}",
            rendered.len()
        );
    }

    #[test]
    fn test_render_integrations_only() {
        let sid = SystemIdentityDocument {
            persona: Some("Atlas".to_owned()),
            integrations: vec![IntegrationSummary {
                name: "notion".to_owned(),
                tool_count: 22,
            }],
            builtin_tools: vec![],
        };

        let rendered = sid.render();
        assert!(rendered.contains("You are Atlas."));
        assert!(rendered.contains("Integrations: notion (22 tools)"));
        assert!(
            !rendered.contains("Built-in tools:"),
            "should not render built-in section when empty"
        );
        assert!(
            !rendered.contains("No tools configured yet"),
            "should not show 'no tools' when integrations exist"
        );
    }

    #[test]
    fn test_build_sid_filters_pending_persona() {
        // The __pending__ sentinel should be treated as None by callers.
        // build_sid itself doesn't filter — the caller (rebuild_sid in main.rs)
        // passes None. Verify that passing None produces no persona in output.
        let sid = build_sid(None, &[("email".to_owned(), 3)], &[]);
        let rendered = sid.render();
        assert!(
            !rendered.starts_with("You are"),
            "SID should not start with persona when None (filtered __pending__)"
        );
        assert!(sid.persona.is_none(), "persona field should be None");
        assert!(rendered.contains("email"));
    }

    #[test]
    fn test_render_omits_admin_tool() {
        let tool_summaries = vec![("admin".to_owned(), 10), ("email".to_owned(), 3)];
        let sid = build_sid(None, &tool_summaries, &[]);

        let rendered = sid.render();
        assert!(!rendered.contains("admin"));
        assert!(rendered.contains("email"));
    }
}
