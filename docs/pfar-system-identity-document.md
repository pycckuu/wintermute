# PFAR Architectural Gap: System Identity Document

> **What**: PFAR forgets who it is after every restart — not just integrations, everything  
> **Root cause**: Missing architectural concept — no unified runtime context assembled from disk  
> **Fix**: System Identity Document (SID) — dynamically assembled, injected into every LLM call  
> **Priority**: Phase 1 — nothing works properly without this

---

## 1. The Real Problem

The Telegram trace shows MCP amnesia, but the problem is universal. After restart, PFAR doesn't know:

| What's forgotten | Where it's persisted | Why it's lost |
|---|---|---|
| What integrations are active | `~/.pfar/mcp/*.toml` + vault | No respawn at startup |
| What persona/name the agent has | `memory.db` persona table | Loaded only by synthesizer, not at boot |
| What happened in recent conversations | `sessions.db` | Loaded per-request but LLM gets no summary |
| What the owner's preferences are | `memory.db` | Injected into planner/synth but not into fast path |
| What scheduled jobs exist | `config.toml` | Scheduler starts, but LLM doesn't know about jobs |
| What self-created skills exist | `~/.pfar/skills/` | No respawn at startup |
| What channels are connected | Adapter state in `sessions.db` | Adapters reconnect, but LLM doesn't know which |
| What the agent can do at all | Tool registry (runtime only) | Rebuilt at startup but not communicated to LLM |

**Every piece of state is persisted somewhere.** The data survives restarts. What's missing is:

1. **A boot sequence that reconstructs the full runtime** from all those persistence points
2. **A unified context document that tells the LLM what PFAR is right now** — injected into every call

OpenClaw doesn't have this problem because it reads `.clawdbot/rules`, skill directories, and workspace files on every activation. Claude Code reads `CLAUDE.md`. These are **system identity documents** — they tell the LLM "here's who you are, what you can do, and what context you have."

PFAR has no equivalent. Each component loads its own state independently, but nothing assembles the full picture and hands it to the LLM. The LLM operates blind — it's a stateless function that gets a narrow prompt and has to guess what system it's running inside.

---

## 2. The Fix: System Identity Document (SID)

A SID is a structured text block, dynamically assembled from disk state at boot and updated as state changes, injected as the **first section of every LLM system prompt** — Planner, Synthesizer, fast path, all of them.

It's not a file on disk. It's a runtime object that the kernel assembles by reading all persistence points.

### What it contains

```
You are Atlas, a personal AI assistant for Igor.
Communication style: concise, direct, no fluff.

CAPABILITIES:
- Integrations: notion (22 tools — pages, search, comments), github (51 tools — issues, PRs, repos)
- Custom skills: uptime-checker (2 tools), csv-parser (1 tool)
- Channels: telegram (active), slack (active), webhooks (listening)
- Scheduled jobs: daily email digest at 08:00, weekly github summary on Monday
- Tools: email, calendar, browser, scripts

RECENT CONTEXT:
- Last conversation (2 hours ago): discussed Bali trip planning, flight on March 15th
- Pending: none

RULES:
- Never mention internal architecture (pipeline, kernel, phases, planner, synthesizer)
- You are a personal assistant, not a system component
- When you don't have a tool for something, say so directly and offer to create one
```

### How it's assembled

```rust
pub struct SystemIdentityDocument {
    persona: Option<PersonaConfig>,         // from memory.db
    active_integrations: Vec<Integration>,  // from MCP manager runtime state
    custom_skills: Vec<SkillSummary>,       // from skill index
    active_channels: Vec<ChannelStatus>,    // from adapter manager
    scheduled_jobs: Vec<JobSummary>,        // from scheduler
    available_tool_categories: Vec<String>, // from tool registry
    recent_context: Option<String>,         // from sessions.db (last N turns summarized)
    pending_flows: Vec<FlowSummary>,        // from flow manager
    owner_preferences: Vec<String>,         // from memory.db
}

impl SystemIdentityDocument {
    /// Assemble from all persistence points. Called at boot and on state changes.
    pub async fn assemble(kernel: &Kernel) -> Result<Self> {
        Ok(Self {
            persona: kernel.vault.get_persona()?,

            active_integrations: kernel.mcp_manager
                .list_active()
                .iter()
                .map(|s| Integration {
                    name: s.name.clone(),
                    tool_count: s.tools.len(),
                    categories: summarize_tool_categories(&s.tools),
                    status: s.status,
                })
                .collect(),

            custom_skills: kernel.skill_index
                .list_all()
                .iter()
                .map(|s| SkillSummary {
                    name: s.name.clone(),
                    tool_count: s.tools.len(),
                })
                .collect(),

            active_channels: kernel.adapter_manager
                .list_active()
                .iter()
                .map(|a| ChannelStatus {
                    name: a.name.clone(),
                    connected: a.is_connected(),
                })
                .collect(),

            scheduled_jobs: kernel.scheduler
                .list_jobs()
                .iter()
                .map(|j| JobSummary {
                    description: j.description.clone(),
                    schedule: j.cron_expression.clone(),
                })
                .collect(),

            available_tool_categories: kernel.tool_registry.categories(),

            recent_context: kernel.vault
                .get_recent_summary(&kernel.owner_principal, 3)  // last 3 turns
                .ok(),

            pending_flows: kernel.flow_manager.active_summaries(),

            owner_preferences: kernel.vault
                .get_memory_entries("preferences")
                .unwrap_or_default(),
        })
    }

    /// Render to text for prompt injection (~100-200 tokens)
    pub fn render(&self) -> String {
        let mut out = String::new();

        // Persona
        match &self.persona {
            Some(p) => out.push_str(&format!("You are {}.\n{}\n", p.name, p.style)),
            None => out.push_str("You are a personal AI assistant on first run. Ask the owner to configure your name and style.\n"),
        }

        // Capabilities
        out.push_str("\nCAPABILITIES:\n");
        if !self.active_integrations.is_empty() {
            let integ: Vec<String> = self.active_integrations.iter()
                .map(|i| format!("{} ({} tools — {})", i.name, i.tool_count, i.categories))
                .collect();
            out.push_str(&format!("- Integrations: {}\n", integ.join(", ")));
        }
        if !self.custom_skills.is_empty() {
            let skills: Vec<String> = self.custom_skills.iter()
                .map(|s| format!("{} ({} tools)", s.name, s.tool_count))
                .collect();
            out.push_str(&format!("- Custom skills: {}\n", skills.join(", ")));
        }
        if !self.active_channels.is_empty() {
            let chans: Vec<String> = self.active_channels.iter()
                .map(|c| format!("{} ({})", c.name, if c.connected { "active" } else { "reconnecting" }))
                .collect();
            out.push_str(&format!("- Channels: {}\n", chans.join(", ")));
        }
        if !self.scheduled_jobs.is_empty() {
            let jobs: Vec<String> = self.scheduled_jobs.iter()
                .map(|j| format!("{} — {}", j.description, j.schedule))
                .collect();
            out.push_str(&format!("- Scheduled: {}\n", jobs.join(", ")));
        }

        // Recent context
        if let Some(ctx) = &self.recent_context {
            out.push_str(&format!("\nRECENT CONTEXT:\n{}\n", ctx));
        }

        // Pending
        if !self.pending_flows.is_empty() {
            out.push_str("\nPENDING:\n");
            for flow in &self.pending_flows {
                out.push_str(&format!("- {} setup: {}\n", flow.service, flow.state_description));
            }
        }

        // Rules (always present)
        out.push_str("\nRULES:\n");
        out.push_str("- Never mention internal architecture (pipeline, kernel, phases, planner, synthesizer, extractor)\n");
        out.push_str("- You are a personal assistant, not a system component\n");
        out.push_str("- When you lack a tool for something, say so directly and offer to create one\n");
        out.push_str("- When the owner mentions a connected service by name, use it\n");

        out
    }
}
```

### Token budget

| Section | Typical size |
|---|---|
| Persona | ~20 tokens |
| Integrations (3 services) | ~40 tokens |
| Skills (2 custom) | ~15 tokens |
| Channels (2 active) | ~15 tokens |
| Scheduled jobs (2) | ~20 tokens |
| Recent context (3-turn summary) | ~60 tokens |
| Rules | ~50 tokens |
| **Total** | **~220 tokens** |

Negligible. Even at 10 services + 10 skills + 5 jobs, this stays under 400 tokens.

---

## 3. Where the SID Gets Injected

Every. Single. LLM. Call.

```rust
impl Pipeline {
    fn build_system_prompt(&self, phase: Phase, task: &Task) -> String {
        let sid = self.sid.render();  // always first

        match phase {
            Phase::Extract => format!("{}\n\n{}", sid, EXTRACTOR_PROMPT),
            Phase::Plan    => format!("{}\n\n{}", sid, PLANNER_PROMPT),
            Phase::Synth   => format!("{}\n\n{}", sid, SYNTH_PROMPT),
            Phase::FastPath => format!("{}\n\n{}", sid, FAST_PATH_PROMPT),
        }
    }
}
```

Currently, each phase has its own system prompt with no shared context. The Extractor doesn't know what tools exist. The fast-path Synthesizer doesn't know what integrations are active. The Planner knows about tools (via `available_tools`) but not about persona or scheduled jobs.

The SID unifies this. Every phase sees the same base context. Each phase still adds its own role-specific instructions after the SID.

---

## 4. When the SID Updates

The SID is not rebuilt on every request (wasteful). It's rebuilt on **state changes**:

```rust
impl Kernel {
    /// Called on any state change that affects the SID
    async fn refresh_sid(&self) {
        self.sid = SystemIdentityDocument::assemble(self).await
            .unwrap_or_else(|e| {
                log::warn!("SID assembly failed: {}. Using stale.", e);
                self.sid.clone()
            });
    }
}
```

Triggers:
- **Startup** (always — full assembly from disk)
- **MCP server spawned/stopped** (integration added/removed)
- **Skill created/updated/deleted** (custom capabilities changed)
- **Persona configured** (first-time or changed)
- **Adapter connected/disconnected** (channel status changed)
- **Cron job added/removed** (scheduled capabilities changed)
- **Memory consolidated** (daily — recent context changes)

NOT on every message (conversation history is per-request anyway).

---

## 5. Boot Sequence (Revised)

The Persistence & Recovery spec's startup sequence needs a complete rewrite. Current version loads components independently with no assembly step. New version:

```rust
pub async fn startup(&self) -> Result<()> {
    // ── Phase A: Restore persistent state ──

    // 1. Load config
    let config = Config::load_from_disk()?;

    // 2. Unlock vault (master key from OS keychain)
    self.vault.unlock(&config.vault)?;

    // 3. Start audit logger
    self.audit_logger.start()?;

    // 4. Load policy engine, tool registry, templates
    self.policy_engine.load(&config.data_flow)?;
    self.tool_registry.load(&config)?;
    self.template_registry.load_from_dir(&config.templates_dir)?;
    self.inference_proxy.start(&config.llm).await?;

    // ── Phase B: Reconstruct runtime from disk ──

    // 5. Kill orphaned containers from before crash
    let orphans = self.container_manager.kill_all_managed().await?;

    // 6. Respawn persisted MCP servers
    let mut respawned_integrations = vec![];
    for config_path in read_dir("~/.pfar/mcp/")?.filter_toml() {
        let mcp_config: McpServerConfig = load_toml(&config_path)?;
        if self.vault.has_all_credentials(&mcp_config.auth) {
            match self.mcp_manager.spawn_server(mcp_config.clone()).await {
                Ok(tools) => {
                    respawned_integrations.push((mcp_config.name.clone(), tools.len()));
                    log::info!("Respawned {} ({} tools)", mcp_config.name, tools.len());
                }
                Err(e) => log::warn!("Failed to respawn {}: {}", mcp_config.name, e),
            }
        }
    }

    // 7. Respawn self-created skills
    let mut respawned_skills = vec![];
    for skill_dir in read_dir("~/.pfar/skills/")? {
        let skill_config = load_skill_config(&skill_dir)?;
        match self.mcp_manager.spawn_server(skill_config.clone().into()).await {
            Ok(tools) => {
                self.skill_index.add(&skill_config, &tools).await?;
                respawned_skills.push(skill_config.name.clone());
            }
            Err(e) => log::warn!("Skill {} failed: {}", skill_config.name, e),
        }
    }

    // 8. Start scheduler (loads cron jobs from config)
    self.scheduler.start(&config)?;

    // ── Phase C: Assemble identity ──

    // 9. Build System Identity Document from all loaded state
    self.sid = SystemIdentityDocument::assemble(self).await?;
    log::info!("SID assembled: {} integrations, {} skills, {} channels",
        self.sid.active_integrations.len(),
        self.sid.custom_skills.len(),
        self.sid.active_channels.len(),
    );

    // ── Phase D: Go live ──

    // 10. Start adapters (primary channel first)
    self.start_adapters(&config).await;

    // 11. Start container reconciliation loop
    self.container_manager.start_reconciliation_loop();

    // 12. Notify owner with actual state
    let msg = self.format_startup_notification(
        orphans, &respawned_integrations, &respawned_skills
    );
    self.notify_owner(&msg).await?;

    Ok(())
}

fn format_startup_notification(
    &self,
    orphans: usize,
    integrations: &[(String, usize)],
    skills: &[String],
) -> String {
    let mut parts = vec!["System restarted.".to_string()];

    if orphans > 0 {
        parts.push(format!("Cleaned up {} orphaned containers.", orphans));
    }

    if !integrations.is_empty() {
        let summary: Vec<String> = integrations.iter()
            .map(|(name, count)| format!("{} ({} tools)", name, count))
            .collect();
        parts.push(format!("Active: {}.", summary.join(", ")));
    }

    if !skills.is_empty() {
        parts.push(format!("{} custom skills loaded.", skills.len()));
    }

    if integrations.is_empty() && skills.is_empty() {
        parts.push("No integrations configured yet.".to_string());
    }

    parts.push("If you were waiting on something, just ask again.".to_string());
    parts.join(" ")
}
```

The key change: **Phase B (reconstruct) and Phase C (assemble identity) didn't exist before.** The old startup went straight from "load components" to "start adapters." Now there's an explicit reconstruction step that rebuilds the full runtime state from disk.

---

## 6. What Changes in Each Spec

### Persistence & Recovery (Phase 1)
- Replace §3 startup sequence with the one above
- Add SID assembly as a startup step
- Add respawn loops for MCP servers and skills
- Replace generic restart message with state-aware notification

### Main Spec (§10 Prompt Strategy, §13)
- Add SID as first section of every system prompt template
- Extractor gets SID (knows what services are available → better entity extraction)
- Planner gets SID (knows capabilities beyond just tool list)
- Synthesizer gets SID (stops hallucinating about what PFAR can do)
- Fast path gets SID (critically — currently has zero context)

### Pipeline Fast Path (Phase 2)
- Fast-path synthesizer prompt must include SID
- Currently: `{extraction} + {conversation_history}` → synthesize
- After: `{SID} + {extraction} + {conversation_history}` → synthesize
- This alone fixes most hallucination ("I don't have a Notion integration")

### Persona & Onboarding (Phase 2)
- Persona is no longer a standalone injection — it's part of the SID
- Onboarding still writes to `memory.db`, but the SID picks it up automatically
- Remove the separate persona injection logic from synthesizer

### Dynamic Integrations (Phase 3)
- After MCP server spawn/stop: `kernel.refresh_sid()`
- After KernelFlow completes: `kernel.refresh_sid()`
- Description overrides loaded at spawn time (from previous analysis)

### Self-Extending Skills (Phase 4)
- After skill deploy/update/delete: `kernel.refresh_sid()`
- Skill index is a data source for SID assembly

### Memory System (Phase 3)
- Daily consolidation updates the "recent context" section of SID
- Owner preferences from memory feed into SID
- After memory writes: `kernel.refresh_sid()` (debounced)

---

## 7. The OpenClaw Comparison

| Concept | OpenClaw | Claude Code | PFAR (before) | PFAR (with SID) |
|---|---|---|---|---|
| Identity source | `.clawdbot/rules` + Custom Instructions | `CLAUDE.md` + project config | Scattered across 4 DBs + TOML files | SID assembled from all sources |
| When loaded | Every task activation | Every session start | Never fully assembled | Boot + state changes |
| What it tells the LLM | Rules, skills, MCP servers, user prefs | Project context, coding style, rules | Narrow per-phase prompt only | Full system state in every call |
| Survives restart | Yes (filesystem) | Yes (filesystem) | Data survives, assembly doesn't | Yes (reconstruct from disk) |
| Token cost | ~500-2000 (skills can be large) | ~200-1000 | 0 (no system context) | ~200-400 |

The core pattern is the same across all working systems: **read state from disk → assemble into a text document → inject into every LLM call.** PFAR stores the state but never assembles or injects it.

---

## 8. Cascading Effects

Once the SID exists, several other problems resolve automatically:

**Hallucination about capabilities** → gone. The LLM knows exactly what's available because the SID says so. No more "I don't have a Notion integration" when Notion has 22 tools.

**"What can you do?"** → answerable. The SID lists all capabilities. The synthesizer can give a real answer instead of generic AI assistant boilerplate.

**Fast path quality** → dramatically better. Currently the fast-path synthesizer has no context about the system. With SID, even without tool calls, it knows "the owner uses Notion, has an uptime checker, gets daily email digests" and can reference these in conversation.

**Entity extraction** → improved. The Extractor sees the SID and knows which service names to look for. "in notion" becomes recognizable as a reference to the active Notion integration, not a random word.

**Implicit skill creation** → possible. When the Planner sees from the SID that no matching tool exists, it can suggest creation. Without the SID, it doesn't even know what tools exist to know what's missing.

**Multi-turn coherence** → better. The "recent context" section gives every LLM call a brief summary of what was just discussed, even across restarts. Not full conversation replay, but enough for "we were just talking about Bali flights."

---

## 9. Implementation Checklist

### SID Core
- [ ] `SystemIdentityDocument` struct with all fields
- [ ] `assemble()` — reads from vault, MCP manager, skill index, scheduler, adapters
- [ ] `render()` — produces ~200-400 token text block
- [ ] `refresh_sid()` on kernel — called on state changes
- [ ] Unit tests: assemble with various combinations of state

### Prompt Integration
- [ ] `build_system_prompt()` prepends SID to every phase's prompt
- [ ] Extractor system prompt: `{SID}\n\n{EXTRACTOR_INSTRUCTIONS}`
- [ ] Planner system prompt: `{SID}\n\n{PLANNER_INSTRUCTIONS}\n\n{available_tools}`
- [ ] Synthesizer system prompt: `{SID}\n\n{SYNTH_INSTRUCTIONS}`
- [ ] Fast-path system prompt: `{SID}\n\n{FAST_PATH_INSTRUCTIONS}`

### Boot Reconstruction
- [ ] MCP server respawn loop (scan `~/.pfar/mcp/`, check vault, spawn)
- [ ] Skill respawn loop (scan `~/.pfar/skills/`, spawn, re-index)
- [ ] SID assembly after all respawns complete
- [ ] State-aware startup notification

### State Change Triggers
- [ ] `refresh_sid()` after MCP spawn/stop
- [ ] `refresh_sid()` after skill create/update/delete
- [ ] `refresh_sid()` after persona configuration
- [ ] `refresh_sid()` after adapter connect/disconnect
- [ ] `refresh_sid()` after cron job add/remove
- [ ] `refresh_sid()` after daily memory consolidation
- [ ] Debounce: max once per 5 seconds (batch rapid changes)

### Tests
- [ ] Boot with 3 integrations + 2 skills → SID lists all
- [ ] Restart → MCP servers respawned → SID updated → LLM knows about them
- [ ] "What can you do?" → answer references actual capabilities from SID
- [ ] Fast path with Notion connected → no hallucination about lacking Notion
- [ ] Add integration mid-session → SID refreshes → next LLM call sees it
- [ ] Remove skill → SID refreshes → LLM stops offering it
- [ ] Empty state (fresh install) → SID triggers onboarding
- [ ] SID < 500 tokens with 5 integrations + 5 skills + 3 jobs
