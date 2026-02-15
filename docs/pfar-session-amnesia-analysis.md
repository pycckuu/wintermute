# PFAR Session Amnesia: Gap Analysis & Fixes

> **What**: The agent forgets everything after every restart — connected integrations, conversation context, system state  
> **Root cause**: Not one bug but 4 overlapping failures in the existing design  
> **Scope**: Patches to existing specs, not a new feature

---

## The Trace, Annotated

I'm numbering each failure class with a tag. Every line of the trace maps to one of **[F1]–[F4]**.

### Attempt 1 (23:00)

```
Igor: Setup notion
PFAR: [gives setup instructions, asks for token]         ← OK, KernelFlow activated
Igor: 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
PFAR: "appears to be an internal identifier"              ← [F1] CREDENTIAL INTERCEPTION FAILED
```

**[F1] Why**: The token paste reached the pipeline instead of being intercepted by KernelFlow. The `flow_manager.intercept()` check either didn't fire, or the Extractor consumed the message first and sent it to fast path. The fast-path synthesizer saw a random string and hallucinated.

### Attempt 2 (23:13) — after restart

```
Igor: Lets setup notion
PFAR: [gives setup instructions again]                    ← [F2] FLOW STATE LOST
PFAR: "notion credential stored securely."                ← credential somehow arrived
Igor: Ok, setup notion
PFAR: [gives setup instructions AGAIN]                    ← [F2][F3] doesn't know cred is stored
```

**[F2] Why**: KernelFlow state was in-memory. Restart killed it. The agent doesn't know a credential was just stored because no persistent record of "notion: credential acquired, server not yet spawned" exists.

**[F3] Why**: The synthesizer's system prompt has no information about what's in the vault or what integrations are configured. It generates responses from training data alone.

### Attempt 3 (00:22–00:23) — after restart

```
Igor: setup notion
PFAR: "Paste the token here when ready"                   ← KernelFlow worked this time
PFAR: "credential stored, but failed to start server:
       MCP server closed stdout unexpectedly"             ← MCP server crash (separate bug)
Igor: connect notion
PFAR: "Failed to connect notion: MCP server closed
       stdout unexpectedly"                               ← retry also fails (MCP issue)
```

This is a real MCP server bug (likely a missing npm dependency or node version issue in the sandbox). Not a design problem — but the agent should remember this failure state.

### Attempt 4 (00:30) — the "success" that fails

```
Igor: connect notion
PFAR: "Paste the token here"                              ← KernelFlow worked
PFAR: "notion connected! Discovered 22 tools:
       notion.API-get-user, notion.API-post-search..."    ← SUCCESS
Igor: check my pages
PFAR: "I don't have access to a tool that lets me
       view or retrieve your pages right now."            ← [F4] PLANNER BLIND TO TOOLS
Igor: in nition
PFAR: "I'm not sure what 'in nition' refers to"          ← [F4] fast path, no tool awareness
Igor: notion
PFAR: "Are you asking about getting started with
       Notion?"                                           ← [F3] hallucinating from training data
Igor: print my pages
PFAR: "I'd be happy to help you print your pages"         ← [F3] hallucinating about printers
```

**[F4] Why**: 22 tools were discovered and registered, but the Planner either never saw them (not in `available_tools` in the prompt), or couldn't map "check my pages" to `notion.API-post-search` (the naming problem from previous research).

### Attempt 5 (10:15) — after restart

```
Igor: Connect to notion
PFAR: "I don't currently have a Notion integration
       tool in my available capabilities."                ← [F2] MCP NOT RESPAWNED
```

**[F2] Why**: `~/.pfar/mcp/notion.toml` should exist on disk. Credential should be in vault. The startup sequence should have spawned it. It didn't — either the respawn code isn't implemented, or it failed silently.

```
Igor: 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
PFAR: "appears to be an internal identifier"              ← [F1] CREDENTIAL INTERCEPTION FAILED AGAIN
```

**[F1] again**: Exact same failure. No active KernelFlow → token goes to pipeline → fast path → hallucination.

---

## The 4 Failure Classes

### F1: Credential interception doesn't work reliably

**What happens**: Owner pastes a token. No active KernelFlow exists (either none was started, or it was lost to restart). The token goes to the normal pipeline. Fast path sees a random string, hallucinates.

**What the spec says**: Dynamic Integrations §5.5 defines in-chat paste detection:
```rust
fn try_intercept_credential(&self, msg: &InboundMessage) -> bool {
    // Check all active flows for credential patterns
    for flow in &self.active_flows { ... }
}
```

**The gap**: This only works if a flow is already active. If there's no flow (restart killed it, or owner just pastes cold), the token falls through. The spec has **no fallback detection** for bare credential pastes.

**Fix**: Add a pre-pipeline credential pattern detector that runs even without an active flow:

```rust
// In main loop, BEFORE pipeline entry
if flow_manager.intercept(&msg).await { continue; }

// NEW: cold credential detection
if let Some(service) = detect_credential_pattern(&msg.text) {
    // Recognized pattern (ntn_*, xoxb-*, ghp_*, sk-*)
    // Auto-start a KernelFlow for this service
    flow_manager.start_integration_setup_from_credential(&service, &msg).await?;
    continue;
}
```

The pattern table already exists in the registry (§9, `credential_pattern` field in known service configs). Wire it up as a standalone check, not just inside active flows.

### F2: MCP servers don't survive restarts

**What happens**: After restart, connected integrations are gone. The agent says "I don't have a Notion integration" even though `notion.toml` is on disk and the credential is in vault.

**What the spec says**: The checklist item exists (Dynamic Integrations §13): "Load `~/.pfar/mcp/*.toml` at startup, spawn existing servers." The Self-Extending Skills spec (§14) also has startup loading.

**The gap**: This is listed as a checkbox but not specified as a startup step. The Persistence & Recovery spec's startup sequence (§3) says `self.tool_registry.load(&config)?` but doesn't mention MCP servers. There's no step for "scan `~/.pfar/mcp/`, check vault for credentials, spawn servers."

**Fix**: Add explicit step to the Persistence & Recovery startup sequence:

```rust
pub async fn startup(&self) -> Result<()> {
    // ... existing steps 1-5 ...

    // NEW: Step 5.5 — Respawn persisted MCP servers
    let mcp_configs = read_dir("~/.pfar/mcp/")
        .filter(|f| f.extension() == "toml");

    for config_path in mcp_configs {
        let config: McpServerConfig = load_toml(&config_path)?;

        // Check vault has the required credentials
        if self.vault.has_all_credentials(&config.auth) {
            match self.mcp_manager.spawn_server(config).await {
                Ok(tools) => {
                    log::info!("Respawned {} with {} tools", config.name, tools.len());
                    self.skill_index.add(&config, &tools).await?;
                }
                Err(e) => {
                    log::warn!("Failed to respawn {}: {}. Owner can retry.",
                        config.name, e);
                    // Don't crash startup — log and continue
                }
            }
        } else {
            log::info!("Skipping {} — credentials not in vault", config.name);
        }
    }

    // NEW: Step 5.6 — Respawn self-created skills
    let skill_dirs = read_dir("~/.pfar/skills/");
    for skill_dir in skill_dirs {
        let config = load_skill_config(&skill_dir)?;
        match self.mcp_manager.spawn_server(config.into()).await {
            Ok(tools) => self.skill_index.add(&config, &tools).await?,
            Err(e) => log::warn!("Skill {} failed to start: {}", config.name, e),
        }
    }

    // ... existing steps 6-8 ...
}
```

The startup notification should also report what was respawned:

```
"System restarted. Active integrations: notion (22 tools), github (51 tools).
 If you were waiting on something, just ask again."
```

Instead of the generic message that tells the owner nothing.

### F3: Synthesizer has no system state awareness

**What happens**: After restart (or even mid-session), the synthesizer hallucinates because it doesn't know what PFAR can actually do. It says "I don't have access to a Notion integration" when Notion is connected with 22 tools. It suggests "use Ctrl+P to print" when the owner asks to list Notion pages.

**What the spec says**: The Planner prompt includes `available_tools` (§10.3 in main spec). But the fast-path synthesizer sees **none of this** — it gets only the extracted metadata and conversation history.

**The gap**: There's no **system context block** in the synthesizer's prompt that tells it what PFAR currently has available. The synthesizer operates blind — it doesn't know what integrations are active, what tools exist, or what the agent is capable of.

**Fix**: Add a `system_context` block that the kernel injects into EVERY LLM call (Planner AND Synthesizer, full path AND fast path):

```rust
fn build_system_context(&self) -> SystemContext {
    SystemContext {
        active_integrations: self.mcp_manager.list_active()
            .map(|s| IntegrationSummary {
                name: s.name.clone(),
                tool_count: s.tools.len(),
                status: s.status,
            })
            .collect(),

        available_tool_categories: self.tool_registry.categories(),
        // e.g. ["notion (pages, search, comments)", "github (issues, PRs)", "email"]

        pending_flows: self.flow_manager.active_summaries(),
        // e.g. ["notion setup: awaiting credential"]

        skill_count: self.skill_index.count(),
    }
}
```

Injected into every prompt as a compact block:

```
SYSTEM STATE:
- Active integrations: notion (22 tools), github (51 tools)
- Self-created skills: uptime-checker, csv-parser (4 tools total)
- Pending: none
- Available capabilities: page search, page read/write, issue management,
  email, calendar, browser, scripts
```

This is ~50 tokens. Negligible cost. Massive impact — the synthesizer stops hallucinating about capabilities because it KNOWS what's available.

**Critical**: This block must also be in the fast-path synthesizer prompt. Currently fast path skips the Planner and goes straight to Synthesize with no tool awareness. The system context block fixes this without adding a Planner call.

### F4: Planner can't find/use discovered tools

**What happens**: 22 Notion tools are registered (`notion.API-post-search`, `notion.API-get-block-children`, etc.). Owner says "check my pages." Planner either doesn't see the tools or can't map the request to them.

**What the spec says**: Planner gets `available_tools` in its prompt. But the tool names are API-route-derived, not semantic.

**The gap**: Two sub-problems:

**F4a — Tool descriptions are API docs, not usage triggers.** The Notion MCP server returns descriptions like "Query a database" for `notion.API-query-data-source`. The Planner needs "Search for pages, databases, or content in your Notion workspace. Use when the user wants to find, list, or look up anything in Notion."

**F4b — No semantic pre-filter.** With 22 Notion tools + built-in tools, the Planner is in the degradation zone (10-20+ tools). The Self-Extending Skills spec (§9) introduces semantic retrieval, but this is Phase 4. The current pipeline dumps all registered tools into the prompt.

**Fix (immediate — no new infrastructure)**:

Add a **description enrichment table** in the MCP registry. When a known server is connected, override its raw descriptions with LLM-optimized ones:

```toml
# ~/.pfar/mcp/description_overrides/notion.toml

[overrides]
"notion.API-post-search" = """
Search Notion workspace. Use when user wants to find pages, databases, or content.
Triggers: "search notion", "find in notion", "my pages", "check my pages", "look up"
NOT for: creating, editing, or deleting content.
"""

"notion.API-retrieve-a-page" = """
Get a specific Notion page by ID. Use when user mentions a specific page or
when you have a page ID from a search result.
"""

"notion.API-post-page" = """
Create a new page in Notion. Use when user wants to create, write, or add a new page.
Triggers: "create page", "new page", "add to notion", "write in notion"
"""
```

Ship override files for the ~20 known services in the registry. For unknown MCP servers (custom or self-created), the LLM generates descriptions at connection time (add a step after `tools/list` in KernelFlow).

**Fix (Phase 4 — from Self-Extending Skills spec)**:

Semantic pre-filter with embeddings. "check my pages" → embedding → cosine similarity → top 5 tools → Planner sees only those. Already designed in §9 of the skills spec.

---

## Why OpenClaw Doesn't Have This Problem

| Dimension | OpenClaw | PFAR (current) | PFAR (with fixes) |
|---|---|---|---|
| **Restart frequency** | Rare (VS Code extension, stays running) | Frequent (standalone daemon, crashes) | Same frequency, but survives restarts |
| **State persistence** | Workspace files (`.clawdbot/`, `SKILL.md`) | Vault DBs + TOML files (designed, gaps in implementation) | Vault + TOML + startup respawn |
| **Tool awareness** | Skills loaded from filesystem every session, injected into system prompt | Tools registered at runtime, lost on restart, not in synthesizer prompt | System context block in every prompt |
| **Skill discovery** | Progressive disclosure (~24 tokens per skill in system prompt) | All tools dumped into Planner prompt | Semantic pre-filter (Phase 4) or description overrides (immediate) |
| **Session continuity** | VS Code webview maintains conversation state | Working memory in sessions.db (designed, unclear if injected properly) | Same + system context block |

The key difference: **OpenClaw reconstructs its state from the filesystem on every activation.** It doesn't "remember" — it re-reads. PFAR should do the same: startup → scan disk → spawn servers → inject state into prompts. The data is already persisted; the reconstruction is the missing piece.

---

## Priority Ordering

| Fix | Effort | Impact | When |
|---|---|---|---|
| **F2: MCP respawn on startup** | Small — 50 lines in startup sequence | Critical — without this, every restart breaks everything | Now |
| **F3: System context block** | Small — 30 lines in prompt builder | Critical — stops hallucination about capabilities | Now |
| **F1: Cold credential detection** | Small — regex table + flow auto-start | High — prevents token paste failures | Now |
| **F4a: Description overrides** | Medium — override files for 20 services | High — makes Planner actually work with MCP tools | Now |
| **F4b: Semantic pre-filter** | Large — embedding model, vector index | High — solves tool scaling permanently | Phase 4 |
| **Informative restart message** | Tiny — template change | Nice — owner knows what's active | Now |

---

## Implementation Patches

### Patch 1: Startup respawn (to Persistence & Recovery spec §3)

Add after step 5 ("Start core services"):

```
5.5. Respawn persisted MCP servers
     - Scan ~/.pfar/mcp/*.toml
     - For each: check vault has credentials → spawn in bubblewrap → discover tools
     - Log failures, don't crash startup
     - Count tools for startup notification

5.6. Respawn self-created skills
     - Scan ~/.pfar/skills/*/skill.toml
     - Same spawn logic
     - Re-index skill embeddings
```

Modify step 8 ("Notify owner"):
```
8. Notify owner with state summary
   "System restarted. Active: notion (22 tools), github (51 tools). 2 custom skills loaded.
    If you were waiting on something, just ask again."
```

### Patch 2: System context injection (to main spec §10 / §13)

Add `system_context` to BOTH Planner and Synthesizer prompt templates:

```
# In every prompt template (planner AND synthesizer AND fast-path)

SYSTEM STATE:
{%- for integration in active_integrations %}
- {{ integration.name }}: {{ integration.tool_count }} tools ({{ integration.status }})
{%- endfor %}
{%- if custom_skills > 0 %}
- {{ custom_skills }} custom skills active
{%- endif %}
{%- if pending_flows %}
PENDING SETUP:
{%- for flow in pending_flows %}
- {{ flow.service }}: {{ flow.state_description }}
{%- endfor %}
{%- endif %}
```

### Patch 3: Cold credential detection (to Dynamic Integrations spec §5)

Add to main loop, before pipeline entry:

```rust
// After flow_manager.intercept() but before pipeline
if let Some((service, token)) = detect_cold_credential(&msg.text) {
    // Known credential pattern without active flow
    // Auto-start flow, skip to credential-received state
    msg.delete().await;  // delete from chat immediately
    flow_manager.start_from_credential(&service, token, &msg.principal_id).await?;
    gateway.send(&msg.principal_id,
        &format!("{} credential detected and stored. Connecting...", service)
    ).await;
    continue;
}

fn detect_cold_credential(text: &str) -> Option<(String, String)> {
    let patterns = [
        ("notion", r"^ntn_[a-zA-Z0-9]{40,}$"),
        ("slack",  r"^xoxb-[0-9]+-[0-9]+-[a-zA-Z0-9]+$"),
        ("github", r"^ghp_[a-zA-Z0-9]{36}$"),
        ("openai", r"^sk-[a-zA-Z0-9]{48,}$"),
    ];
    let trimmed = text.trim();
    for (service, pattern) in patterns {
        if Regex::new(pattern).unwrap().is_match(trimmed) {
            return Some((service.to_string(), trimmed.to_string()));
        }
    }
    None
}
```

### Patch 4: Description overrides (to Dynamic Integrations spec §3)

Add to McpServerConfig:

```toml
# In ~/.pfar/mcp/notion.toml
[description_overrides]
enabled = true
file = "~/.pfar/mcp/overrides/notion.toml"
```

The kernel applies overrides after `tools/list` discovery:

```rust
async fn apply_description_overrides(&self, server: &str, tools: &mut Vec<ToolDef>) {
    let override_path = format!("~/.pfar/mcp/overrides/{}.toml", server);
    if let Ok(overrides) = load_toml::<HashMap<String, String>>(&override_path) {
        for tool in tools.iter_mut() {
            if let Some(desc) = overrides.get(&tool.name) {
                tool.description = desc.clone();
            }
        }
    }
}
```

Ship override files for the ~20 known services in the built-in registry. For unknown servers, generate overrides via LLM at connection time (one-time cost, persisted).

---

## Validation: The Trace With All Fixes Applied

```
[PFAR starts]
→ Scan ~/.pfar/mcp/ → found notion.toml
→ Vault has notion_token ✓
→ Spawn notion MCP server in bubblewrap
→ tools/list → 22 tools → apply description overrides
→ Register tools in pipeline + skill index

PFAR: "System restarted. Active: notion (22 tools). Ask again if needed."

Igor: check my pages
→ Phase 0: Extract {intent: "notion_search", entities: []}
→ System context injected: "Active: notion (22 tools)"
→ Phase 1: Planner sees notion.search (overridden description:
   "Search Notion workspace. Use when user wants to find pages...")
→ Plan: [{tool: "notion.API-post-search", args: {query: ""}}]
→ Phase 2: Execute → MCP call → results
→ Phase 3: "Here are your Notion pages: ..."

Igor: in nition  (typo)
→ Phase 0: Extract — fuzzy match "nition" → "notion"
   OR: system context tells synthesizer Notion is active
→ Fast path (if no tool needed): "I can search your Notion workspace.
   What are you looking for?"
→ Full pipeline (if tool detected): same as above

[After restart, owner pastes credential cold]
Igor: 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
→ Cold credential detection: matches ntn_ pattern → "notion"
→ Message deleted from chat
→ Auto-start KernelFlow → credential stored → spawn → connect
→ "Notion credential detected and stored. Connected! 22 tools available."
```

Every failure from the original trace is resolved.
