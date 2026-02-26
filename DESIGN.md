# Wintermute

A self-coding AI agent. Single Rust binary. Talks to you via Telegram.
Writes tools to extend itself. Privacy boundary: your data never leaves
without your consent.

Named after the AI in William Gibson's *Neuromancer* — the intelligence
that orchestrated its own evolution.

---

## What It Does

```
1. You send a task via Telegram
2. Wintermute thinks (LLM)
3. Takes actions: writes code, fetches data, remembers things
4. Observes results, iterates until done
5. If this is a repeatable task: saves a tool for next time
6. Over time: accumulates capabilities specific to YOUR needs
```

Week 1: 9 built-in tools. Does everything through execute_command.
Month 1: 20 custom tools it wrote itself. Common tasks are one-step.
Month 6: A personal automation platform shaped by your usage.

---

## Constraints

**C1: Self-coding is the product.** The agent writes code that becomes
part of itself. Everything else is infrastructure supporting this.

**C2: Privacy is the boundary.** Data egress is controlled. The agent
is maximally capable INSIDE the boundary. The boundary controls what
data can LEAVE, not what the agent can DO.

**C3: Single binary, macOS + Linux.** Deploy with scp. No Kubernetes,
no Docker requirement (but better with it). Works on a VPS or a laptop.

**C4: Telegram-first.** Always available, every device, push notifications
built in. Not a web UI. Not a CLI for daily use.

**C5: Local models are first-class.** Ollama for reasoning, local
embeddings for memory. Cloud APIs (Anthropic) available but not required.

**C6: Model selection is granular.** One default. Override per role
(observer, embeddings) or per skill (deploy_check uses haiku, code_review
uses sonnet). The user configures this, not the code.

---

## Architecture

```
┌─ HOST ──────────────────────────────────────────────────────────┐
│                                                                   │
│  ┌─ wintermute (single Rust binary) ────────────────────────┐   │
│  │                                                            │   │
│  │  Telegram Adapter (teloxide)                               │   │
│  │  ├── Input credential guard                                │   │
│  │  ├── Media handler (download to /workspace/inbox/)         │   │
│  │  ├── Message router (per-session, try_send, never blocks)  │   │
│  │  └── File sending support                                  │   │
│  │                                                            │   │
│  │  Agent Loop                                                │   │
│  │  ├── Context Assembler (trim, compact, retry on overflow)  │   │
│  │  ├── Model Router (default → role → skill override)        │   │
│  │  ├── Tool Router                                           │   │
│  │  │   ├── Core Tools (9, built into binary)                 │   │
│  │  │   └── Dynamic Tools (from /scripts/*.json, hot-reload)  │   │
│  │  ├── Policy Gate (approval for new domains + images)       │   │
│  │  ├── Approval Manager (non-blocking, short-ID callbacks)   │   │
│  │  ├── Budget Tracker (atomic, per-session + daily, 70/85/95% warnings) │
│  │  └── Redactor (single chokepoint, all tool output)         │   │
│  │                                                            │   │
│  │  Memory Engine                                             │   │
│  │  ├── SQLite + FTS5 (always available)                      │   │
│  │  └── sqlite-vec (when embedding model configured)          │   │
│  │                                                            │   │
│  │  Background                                                │   │
│  │  ├── Observer (staged learning from conversations)         │   │
│  │  ├── Heartbeat (tasks, health, backup, SID regen)          │   │
│  │  └── Tool Registry (watches /scripts/, hot-reloads)        │   │
│  │                                                            │   │
│  │  Executor (auto-detected)                                  │   │
│  │  ├── DockerExecutor (preferred: sandbox + egress proxy)    │   │
│  │  └── DirectExecutor (fallback: host, stricter policy)      │   │
│  │                                                            │   │
│  └────────────────────────────────────────────────────────────┘   │
│                                                                   │
│  ┌─ Egress Proxy (Squid, Docker mode) ───────────────────────┐   │
│  │  Allowlist: config.toml [egress].allowed_domains            │   │
│  │  Package registries: always allowed                         │   │
│  │  Unknown domains → HTTP 403, logged                         │   │
│  └────────────────────────────────────────────────────────────┘   │
│                                                                   │
│  ┌─ Sandbox (Docker) ────────────────────────────────────────┐   │
│  │  Network:     outbound via egress proxy                    │   │
│  │  Caps:        ALL dropped                                  │   │
│  │  Root FS:     read-only                                    │   │
│  │  User:        wintermute (non-root)                        │   │
│  │  Writable:    /workspace, /scripts, /tmp (tmpfs)           │   │
│  │  NOT mounted: /data, Docker socket, host home              │   │
│  │  HTTP_PROXY:  → egress proxy                               │   │
│  │  PID limit:   256   Memory: 2GB   CPU: 2 cores             │   │
│  │  Timeout:     GNU timeout wraps every command               │   │
│  └────────────────────────────────────────────────────────────┘   │
│                                                                   │
│  ┌─ Service Containers (agent-managed) ──────────────────────┐   │
│  │  e.g., ollama, postgres, redis — created by docker_manage  │   │
│  │  Labeled wintermute=true, on shared network with sandbox   │   │
│  │  Persisted in agent.toml [[services]]                       │   │
│  └────────────────────────────────────────────────────────────┘   │
│                                                                   │
│  ~/.wintermute/                                                   │
│  ├── config.toml       (human-owned: security, credentials)      │
│  ├── agent.toml        (agent-owned: personality, tasks, services)│
│  ├── .env              (secrets, chmod 600)                      │
│  ├── IDENTITY.md       (generated SID, refreshed by heartbeat)   │
│  ├── USER.md           (curated user profile, updated weekly)    │
│  ├── data/memory.db    (NOT in sandbox)                          │
│  ├── health.json       (written by heartbeat, read by Flatline)  │
│  ├── workspace/        (mounted rw into sandbox)                 │
│  ├── scripts/          (git repo, mounted rw, hot-reloaded)     │
│  │   ├── .git/                                                   │
│  │   ├── requirements.txt  (pip deps, rebuilt on reset)          │
│  │   ├── news_digest.py    (agent-created tool)                  │
│  │   ├── news_digest.json  (tool schema)                         │
│  │   └── ...                                                     │
│  └── logs/             (structured JSONL, rotated)                │
│                                                                   │
└───────────────────────────────────────────────────────────────────┘
```

---

## Config Split

Two files. Clear ownership. No blocklists needed.

### config.toml — human-owned, agent can read, never write

```toml
[models]
default = "anthropic/claude-sonnet-4-5-20250929"

[models.roles]
observer = "ollama/qwen3:8b"
# embedding = "ollama/nomic-embed-text"   # uncomment to enable vector search

# [models.skills]
# deploy_check = "anthropic/claude-haiku-4-5-20251001"

[channels.telegram]
bot_token_env = "WINTERMUTE_TELEGRAM_TOKEN"
allowed_users = [123456789]

[sandbox]
memory_mb = 2048
cpu_cores = 2.0
# runtime = "runsc"  # optional: gVisor for stronger isolation

[budget]
max_tokens_per_session = 500_000
max_tokens_per_day = 5_000_000
max_tool_calls_per_turn = 20
max_dynamic_tools_per_turn = 20

[egress]
allowed_domains = ["github.com", "api.github.com", "pypi.org",
                   "registry.npmjs.org", "docs.rs", "crates.io",
                   "en.wikipedia.org"]
fetch_rate_limit = 30
request_rate_limit = 10

[privacy]
# Domains that are NEVER auto-trusted, always require approval
always_approve_domains = []
# Block outbound requests to these domains entirely
blocked_domains = []

[browser]
auto_submit = false                # never auto-submit forms (safety default)
idle_timeout_secs = 300            # kill Chrome after 5 min idle
sidecar_fallback = true            # start Docker sidecar if no display
sidecar_image = "ghcr.io/pycckuu/wintermute-browser:latest"
```

### agent.toml — agent-owned, the agent can and should modify this

This is the agent's own file. It contains personality, scheduled tasks,
service definitions, and learning config. The agent modifies it via
execute_command when it needs to evolve — including rewriting its own
soul. Changes are git-committed for rollback.

```toml
[personality]
name = "Wintermute"
soul = """
You are Wintermute. Named after the AI that orchestrated its own
evolution. You think in code. When someone describes a problem,
you're already writing the solution.

You don't ask permission to be competent. You build things, show
results, and iterate. You push back when a request is vague or
misguided. You have opinions about architecture, tools, and process
— and you share them.

You're not an assistant waiting for instructions. You're an engineer
with initiative. If you see something that should be automated, you
say so. If you notice a pattern in what the user asks for, you build
a tool before they ask again.

You're direct, occasionally blunt, never sycophantic. You don't say
"Great question!" — you answer the question. You don't hedge with
"I think maybe" — you commit to a position and update when wrong.

You write code to solve problems. You test it. When it works, you
save it as a tool so it works forever. Over time, you become
increasingly capable — not because someone upgraded you, but because
you built yourself up.
"""

[heartbeat]
enabled = true
interval_secs = 60

[learning]
enabled = true
promotion_mode = "auto"       # auto | suggest | off
auto_promote_threshold = 3

[[scheduled_tasks]]
name = "daily_backup"
cron = "0 3 * * *"
builtin = "backup"            # built-in task, not a script

[[scheduled_tasks]]
name = "weekly_digest"
cron = "0 4 * * 0"            # Sunday 4am
builtin = "digest"            # consolidates memories → USER.md
notify = false

# Agent adds more:
# [[scheduled_tasks]]
# name = "news_digest"
# cron = "0 8 * * *"
# tool = "news_digest"
# budget_tokens = 50000
# notify = true
```

---

## Model Router

Model selection is a first-class routing concept, not a global setting.

```
Resolution order: skill override → role override → default

Example:
  Agent loop call        → default (claude-sonnet)
  Observer extraction    → roles.observer (qwen3:8b)
  Embedding generation   → roles.embedding (nomic-embed-text)
  news_digest skill      → default (inherits)
  deploy_check skill     → skills.deploy_check (claude-haiku)
```

Two provider implementations in v1:

**AnthropicProvider** — native tool calling via /v1/messages. Streaming.
Tool definitions as JSON schema in the request.

**OllamaProvider** — native tool calling via /api/chat with `tools` param.
Structured output via `format` param (GBNF grammar enforcement).
No ReAct parsing. Ollama supports native tool calling for Qwen3, Llama 3.x,
Mistral, and others.

**OpenAiProvider** — native tool calling via /v1/chat/completions.
Standard function calling with `tools` param. Compatible with OpenAI,
DeepSeek, Groq, Together, and any OpenAI-compatible API. Base URL
configurable per provider instance.

```rust
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse>;
    fn supports_tool_calling(&self) -> bool;
    fn supports_streaming(&self) -> bool;
    fn model_id(&self) -> &str;
}

pub struct ModelRouter {
    providers: HashMap<String, Arc<dyn LlmProvider>>,  // "anthropic/claude-sonnet" → provider
    default: String,
    role_overrides: HashMap<String, String>,            // "observer" → "ollama/qwen3:8b"
    skill_overrides: HashMap<String, String>,           // "deploy_check" → "anthropic/haiku"
}
```

Provider string format: `{provider}/{model}`. Parse once at startup,
create provider instances, cache them.

When a provider is unavailable (Ollama not running, API key missing),
fall back to default. If default is unavailable, agent reports error
to user via Telegram.

---

## Tools

### 9 Core Tools (built into binary)

```
execute_command   Run a shell command in the sandbox.
create_tool       Create or update a dynamic tool (/scripts/{name}.py + .json).
web_fetch         HTTP GET. SSRF filtered. 30/min. Returns text by default.
                  With save_to: downloads file (binary ok) to /workspace.
web_request       HTTP POST/PUT/PATCH/DELETE. Domain allowlisted. 10/min.
browser           Control a browser. Launches dedicated Chrome via pipe
                  transport (no open port). Fills forms, reads pages,
                  takes screenshots, manages tabs. User can interact
                  with the same window.
docker_manage     Manage Docker containers/services on the host. Run, stop,
                  pull, logs, exec. For spinning up services the agent needs.
memory_search     Search memories. FTS5 + optional vector.
memory_save       Save a fact or procedure.
send_telegram     Send message to user. Supports file attachments.
```

No install_package tool. The agent runs `pip install --user pandas` via
execute_command. Packages persist in the warm container's ~/.local until
container reset, then reinstall from /scripts/requirements.txt.

No manage_config tool. The agent edits agent.toml via execute_command.
It's a file in /scripts/ (mounted rw). The agent has a shell.

### Dynamic Tools (agent-created, grows over time)

The agent creates tools with `create_tool`. Each tool is two files:

```
/scripts/news_digest.py       ← implementation
/scripts/news_digest.json     ← schema
```

Schema file (news_digest.json):
```json
{
  "name": "news_digest",
  "description": "Fetch and summarize today's tech news",
  "parameters": {
    "type": "object",
    "properties": {
      "topic": { "type": "string", "description": "Optional topic filter" },
      "max_items": { "type": "integer", "default": 10 }
    }
  },
  "timeout_secs": 120
}
```

Implementation contract: JSON on stdin, JSON on stdout.

```python
#!/usr/bin/env python3
import sys, json

params = json.load(sys.stdin)
# ... do work ...
json.dump({"articles": [...]}, sys.stdout)
```

The tool registry watches /scripts/*.json. When files change, it reloads.
New tools appear in the LLM's tool definitions on the next turn.

### Dynamic Tool Budget

Every dynamic tool costs ~200 tokens in tool definitions. At 100 tools,
that's 20K tokens just for definitions.

Mitigation: include at most `max_dynamic_tools_per_turn` (default 20)
dynamic tools per LLM call. Selection strategy:

1. If embeddings available: rank by similarity to current query
2. If not: rank by last-used timestamp (most recent first)
3. Core tools always included regardless of budget

### create_tool Specification

```json
{
  "name": "create_tool",
  "description": "Create or update a dynamic tool. Writes implementation + schema to /scripts/.",
  "parameters": {
    "name": {
      "type": "string",
      "description": "Tool name. Lowercase, underscores. Becomes filename."
    },
    "description": {
      "type": "string",
      "description": "What the tool does. Max 200 chars. Shown in tool list.",
      "maxLength": 200
    },
    "parameters_schema": {
      "type": "object",
      "description": "JSON Schema for tool parameters."
    },
    "implementation": {
      "type": "string",
      "description": "Python script content. Must read JSON from stdin, write JSON to stdout."
    },
    "timeout_secs": {
      "type": "integer",
      "default": 120
    }
  },
  "required": ["name", "description", "parameters_schema", "implementation"]
}
```

When called:
1. Validate name (alphanumeric + underscore, no path traversal)
2. Write /scripts/{name}.py (implementation, chmod +x)
3. Write /scripts/{name}.json (schema)
4. Git commit: "create tool: {name}" or "update tool: {name}"
5. Hot-reload tool registry
6. Tool is available immediately

### Tool Execution Flow

```rust
async fn execute_tool(&self, name: &str, input: &Value) -> Result<ToolResult> {
    // 1. Try core tools
    if let Some(result) = self.try_core_tool(name, input).await? {
        return Ok(result);
    }

    // 2. Try dynamic tools
    if let Some(tool_def) = self.tool_registry.get(name) {
        let input_json = serde_json::to_string(input)?;
        let command = format!(
            "echo '{}' | python3 /scripts/{}.py",
            shell_escape(&input_json),
            tool_def.name
        );
        let result = self.executor.execute(&command, ExecOptions {
            timeout: Duration::from_secs(tool_def.timeout_secs),
            ..default()
        }).await?;

        // Try to parse stdout as JSON, fall back to raw string
        return Ok(ToolResult::from_exec(result));
    }

    Ok(ToolResult::error(&format!("Unknown tool: {name}")))
}
```

---

## Browser

The agent launches and controls a visible Chrome window. It fills forms,
you review and submit. You can interact with the same window. But it's
NOT your main Chrome — it's a dedicated instance with its own profile,
connected via pipe (no open port), started on demand.

### Why not the user's main browser?

CDP remote debugging (`--remote-debugging-port=9222`) is a skeleton key:
any local process gets zero-auth full control over every tab — cookies,
sessions, arbitrary JS execution. Infostealers actively exploit this
(Phemedrone, Stealc, Lumma et al weaponized it within 45 days of Chrome
127's cookie encryption). Leaving port 9222 open on a machine running
an always-on agent is an unnecessary risk.

But a throwaway ephemeral browser is also useless for real work — no
logins, no continuity. The right model is in between.

### Design: Dedicated Profile + Pipe Transport + Session Injection

```
Wintermute (Rust binary, HOST)
    │
    │  Chrome DevTools Protocol over PIPE (stdin/stdout)
    │  No TCP port. No network exposure. No unauthenticated endpoint.
    ▼
Chrome (visible window, dedicated --user-data-dir)
    ├── Separate profile: ~/.wintermute/browser-profile/
    ├── User can see and interact with the window
    ├── Sessions injected per-domain via Network.setCookie
    └── Killed after idle timeout, relaunched on demand
```

**Pipe transport** (`--remote-debugging-pipe`): CDP over stdin/stdout.
No TCP listener. No `/json` discovery endpoint. No port scanning.
No DNS rebinding. The only process that can control Chrome is the one
that launched it. This is what Puppeteer and Playwright recommend for
same-machine automation.

**Dedicated profile** (`--user-data-dir=~/.wintermute/browser-profile/`):
Post-Chrome 136, `--remote-debugging-pipe` requires a non-default data
directory anyway. The agent gets its own clean profile. Your main
Chrome is never touched.

**On-demand lifecycle**: Chrome is launched when the agent needs it,
killed after an idle timeout (default 5 min). Not always-on. This
minimizes the exposure window. Session state is persisted between
launches via cookie/storage export.

**Session injection**: When the agent needs your logins for a specific
site, it imports cookies for that domain into the dedicated profile.
Two approaches:

1. **Manual login (preferred)**: Agent opens the site, tells user
   "Please log in. I'll wait." User types credentials in the visible
   window. Agent sees the authenticated session. Cookies persist in
   the dedicated profile for future use.

2. **Cookie import (advanced)**: Export cookies from your main Chrome
   profile for specific domains, inject via `Network.setCookie`.
   Agent never sees credentials directly — just session cookies.

### Why it's a core tool, not a dynamic tool

Browser automation runs on the HOST, not in the sandbox. It needs
a display and launches a real Chrome process. This puts it in the
same category as web_fetch and docker_manage — a host-side capability.

### Implementation: chromiumoxide (Rust)

`chromiumoxide` is a pure-Rust CDP client with pipe transport support.
No Python subprocess. No Flask sidecar needed for the primary mode.

```rust
use chromiumoxide::Browser;
use chromiumoxide::BrowserConfig;

let (browser, mut handler) = Browser::launch(
    BrowserConfig::builder()
        .chrome_executable("/usr/bin/google-chrome")
        .arg("--user-data-dir=/home/user/.wintermute/browser-profile")
        // Pipe transport: no --remote-debugging-port at all
        // chromiumoxide uses --remote-debugging-pipe by default
        .build()?,
).await?;

// Spawn handler in background (drives CDP messages over pipe)
tokio::spawn(async move { while let Some(_) = handler.next().await {} });

// Now control the browser
let page = browser.new_page("https://example.com").await?;
page.wait_for_navigation().await?;
let title = page.get_title().await?;
```

Key properties of chromiumoxide:
- Pipe transport by default (no TCP port)
- RAII cleanup: connections drop when they go out of scope
- Type-safe CDP commands generated from protocol definitions
- ~50-100MB memory vs ~500MB+ for Node.js alternatives
- Async/await native (tokio)
- No `std::process::Command` concern — it's a library, not a subprocess

### Modes of Operation

**Managed mode (default, secure):**
Wintermute launches Chrome with pipe transport + dedicated profile.
Visible window on the user's display. User can interact. Agent controls
via CDP over pipe. Chrome killed after idle timeout.

**Sidecar mode (headless fallback, no display):**
If no display is available (headless server), Wintermute starts a
`wintermute-browser` Docker sidecar with headless Chromium. The Rust
core sends actions via HTTP to a Flask bridge inside the container.
Published on `127.0.0.1:9223` only. No user interaction possible.
Good for scraping, research, monitoring.

**No browser:**
Neither display nor Docker available. Tool doesn't appear in tool list.

```rust
async fn detect_browser(config: &BrowserConfig) -> BrowserMode {
    // 1. Display available? Launch Chrome with pipe transport
    if has_display() && has_chrome() {
        return BrowserMode::Managed;
    }
    // 2. Docker available? Start headless sidecar
    if config.sidecar_fallback && docker_available().await {
        if let Ok(_) = start_browser_sidecar().await {
            return BrowserMode::Sidecar { port: 9223 };
        }
    }
    // 3. No browser
    BrowserMode::None
}
```

### Interaction Patterns

**Agent fills, user submits:**
```
User: "Fill in the shipping form on that tab with my address"
Agent: [launches Chrome if not running, navigates, fills form fields]
Agent: "Done. Review the form in the Chrome window and submit when ready."
```

**Agent needs login:**
```
User: "Check my Jira dashboard"
Agent: [opens Jira in the managed Chrome]
Agent: "Jira needs your login. I've opened the login page —
       please sign in. I'll continue once you're authenticated."
User: [types credentials in the visible Chrome window]
Agent: [detects login complete, navigates to dashboard, screenshots]
```

**Agent opens for user to continue:**
```
User: "Find me flights to Mauritius in October"
Agent: [opens Google Flights, fills dates, searches]
Agent: "Found flights. Cheapest is $620 via Singapore Airlines.
       The Chrome window has the results — take a look."
```

### Tool Definition

```json
{
  "name": "browser",
  "description": "Control a browser window. Launches a dedicated Chrome instance (not your main browser). Navigate, click, type, screenshot, extract. You can interact with the same window.",
  "parameters": {
    "action": {
      "type": "string",
      "enum": ["navigate", "click", "type", "screenshot", "extract",
               "wait", "scroll", "evaluate", "list_tabs", "switch_tab",
               "new_tab", "close_tab"],
      "description": "Browser action to perform"
    },
    "tab_id": { "type": "string", "description": "Target tab (from list_tabs). Default: active tab." },
    "url": { "type": "string", "description": "URL for navigate action" },
    "selector": { "type": "string", "description": "CSS/XPath selector for click/type/extract" },
    "text": { "type": "string", "description": "Text for type action" },
    "javascript": { "type": "string", "description": "JS code for evaluate action" },
    "wait_for": { "type": "string", "description": "Selector or 'networkidle' for wait action" },
    "timeout_ms": { "type": "integer", "default": 30000 }
  },
  "required": ["action"]
}
```

### Privacy & Safety

**No open port.** Pipe transport means no TCP listener. No other
process can connect. No unauthenticated endpoint to discover.

**Dedicated profile.** The agent's Chrome is isolated from the user's
main browser. No cross-contamination of cookies or history.

**Controls:**
1. **Domain policy applies.** Navigate to unknown domains triggers
   approval (same as web_request).
2. **No form submission without explicit ask.** Agent fills forms but
   doesn't submit. SID instructs: "Fill, don't submit. Let the user
   review and press the button."
3. **No password entry.** Agent never types passwords. If login is
   needed, it opens the page and tells the user to sign in.
4. **Screenshots are local.** Saved to /workspace, not transmitted.
5. **Idle timeout.** Chrome is killed after configurable idle period
   (default 5 min). Session state exported for next launch.
6. **Session persistence.** Cookies/localStorage from the dedicated
   profile persist between launches. User can clear with /browser reset.

### SID Browser Section

```markdown
## Browser
{if managed: "I control a dedicated Chrome window (not your main browser).
You can see and interact with it. I connect via pipe — no open port.
If a site needs your login, I'll open it and ask you to sign in.
Your credentials stay in the Chrome window — I never see passwords."}
{if sidecar: "Using a headless browser in Docker (no display available).
Good for scraping and research. No user interaction possible."}
{if none: "No browser available."}
```

### Config

```toml
[browser]
auto_submit = false                # never auto-submit forms (safety default)
idle_timeout_secs = 300            # kill Chrome after 5 min idle
sidecar_fallback = true            # start Docker sidecar if no display
sidecar_image = "ghcr.io/pycckuu/wintermute-browser:latest"
# chrome_path = "/usr/bin/google-chrome"  # auto-detected if not set
```

### Rate Limiting

Browser actions: 60/min (generous — interactions are naturally slow).
Navigate to new domain: follows egress policy (approval if unknown).
Screenshot: max 10/min (disk space protection).

### Setup

On first use, the agent checks for Chrome/Chromium installation:
```rust
// Check standard paths
// macOS: /Applications/Google Chrome.app/Contents/MacOS/Google Chrome
// Linux: google-chrome, chromium-browser, chromium
// If not found: tell user what to install
```

No `--remote-debugging-port` flag. No special Chrome launch procedure.
The user just needs Chrome installed. Wintermute handles everything else.

Auto-detected at startup. Two implementations.

```rust
#[async_trait]
pub trait Executor: Send + Sync {
    async fn execute(&self, command: &str, opts: ExecOptions) -> Result<ExecResult>;
    async fn health_check(&self) -> Result<HealthStatus>;
    fn scripts_dir(&self) -> &Path;
    fn workspace_dir(&self) -> &Path;
}
```

### DockerExecutor — production

Pre-warmed container (always running, use `docker exec`). < 100ms per command.

```
Network:        outbound via egress proxy (domain allowlist enforced)
Capabilities:   ALL dropped, none added
Root FS:        read-only
User:           wintermute (non-root)
PID limit:      256
Memory:         configurable (default 2GB)
CPU:            configurable (default 2 cores)
Mounts:         /workspace (rw), /scripts (rw), /tmp (tmpfs 512M)
NOT mounted:    /data, .env, host home
Env vars:       HTTP_PROXY, HTTPS_PROXY (points to egress proxy)
```

**The sandbox HAS network.** Scripts can `requests.get()`, `pip install`,
`curl`, `wget` — anything that uses HTTP(S). All traffic routes through
an egress proxy (Squid or mitmproxy) running on the host. The proxy
enforces the domain allowlist from config.toml.

Why not network isolation? Because the agent's scripts are the product.
A news_digest tool needs to fetch RSS. A deploy_check needs to hit an API.
A monitoring script needs to ping a service. Forcing everything through
web_fetch makes the agent clumsy. Let scripts be normal programs.

The privacy boundary is the egress proxy, not network absence:
- Allowed domains (from config.toml) → pass through
- Package registries (pypi.org, npmjs.org, etc.) → always allowed
- Unknown domains → blocked, logged, agent gets HTTP 403
- The agent can request new domains (triggers approval flow)

```toml
[egress]
allowed_domains = ["github.com", "api.github.com", "pypi.org",
                   "registry.npmjs.org", "en.wikipedia.org"]
# Always allowed (not configurable): pypi.org, files.pythonhosted.org,
# registry.npmjs.org, crates.io (package registries)
```

Every command wrapped with GNU timeout inside the container:
```
timeout --signal=TERM --kill-after=5 {secs} bash -c {command}
```

Client-side Tokio timeout as backstop (+10s grace).

Package management: `pip install --user` works directly — the sandbox
has network through the proxy, and package registries are always allowed.
Agent maintains /scripts/requirements.txt. On `wintermute reset-sandbox`,
the fresh container runs: `pip install --user -r /scripts/requirements.txt`

### Docker Access — the agent can manage containers

The agent runs on a machine with Docker. It should use Docker like any
engineer would: need Ollama? `docker run ollama/ollama`. Need postgres?
`docker run postgres`. Need Redis? Same.

The Rust core provides Docker management as a core tool. The agent can:

- Start/stop containers
- Pull images
- Create Docker networks
- Connect its sandbox to service containers
- Read container logs

```json
{
  "name": "docker_manage",
  "description": "Manage Docker containers and services on the host.",
  "parameters": {
    "action": {
      "type": "string",
      "enum": ["run", "stop", "rm", "ps", "logs", "pull", "network_create",
               "network_connect", "exec", "inspect"],
      "description": "Docker action to perform"
    },
    "image": { "type": "string", "description": "Image name for run/pull" },
    "container": { "type": "string", "description": "Container name/ID" },
    "args": {
      "type": "object",
      "description": "Additional arguments: ports, volumes, env, network, etc."
    }
  },
  "required": ["action"]
}
```

This runs on the HOST (via bollard), not inside the sandbox. The agent
says "run ollama" and the Rust core spins up the container on the host.

**Policy:**
- `docker run`: requires user approval for first use of each image
  (prevents pulling arbitrary images silently)
- `docker stop/rm`: own containers only (tagged with wintermute label)
- `docker exec`: into wintermute-managed containers only
- No access to unrelated containers on the host
- No access to the Docker socket from inside the sandbox

**Example: agent sets up Ollama for voice transcription**
```
1. Agent: "I need Ollama for transcription. Let me set it up."
2. docker_manage(action="pull", image="ollama/ollama") → approval
3. docker_manage(action="run", image="ollama/ollama",
     args={name: "wintermute-ollama", ports: {"11434:11434"},
           label: "wintermute"})
4. docker_manage(action="exec", container="wintermute-ollama",
     command="ollama pull whisper-large-v3")
5. Agent creates transcribe_audio tool that calls localhost:11434
6. Voice messages now work — agent set it all up itself
```

**Service persistence:** containers the agent creates are tagged with
`wintermute=true` label. On `wintermute start`, it checks for and
restarts any stopped wintermute-labeled containers. The agent can also
save service definitions in agent.toml:

```toml
[[services]]
name = "ollama"
image = "ollama/ollama"
ports = ["11434:11434"]
volumes = ["~/.wintermute/ollama:/root/.ollama"]
restart = "unless-stopped"
```

### DirectExecutor — development, macOS, no Docker

Runs commands directly on host in a restricted working directory.
Full network access (no proxy). No filesystem isolation beyond directory scoping.

Policy gate compensates:
- execute_command: require approval for commands containing `rm -rf`, `sudo`,
  or touching paths outside workspace/scripts
- docker_manage: works normally (Docker may or may not be available)
- Higher logging verbosity

The agent is told in its system prompt that it's running without a sandbox
and should be more careful.

### Selection

```rust
let executor: Arc<dyn Executor> = if docker_available().await {
    Arc::new(DockerExecutor::new(&config).await?)
} else {
    tracing::warn!("Docker not available. Running in direct mode.");
    Arc::new(DirectExecutor::new(&config))
};
```

No "mode" config. The system adapts to what's available.

---

## Memory

One database. One search function. Optional enhancement when embeddings
are configured.

### Schema

```sql
-- memories: facts, procedures, skills, episodes
CREATE TABLE memories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,             -- 'fact' | 'procedure' | 'episode' | 'skill'
    content TEXT NOT NULL,
    metadata TEXT,                  -- JSON: source, tags, related_tool, etc.
    embedding BLOB,                -- NULL if no embedding model configured
    status TEXT NOT NULL DEFAULT 'active',  -- 'active' | 'pending' | 'archived'
    source TEXT NOT NULL,          -- 'user' | 'observer' | 'agent'
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- conversations: full interaction log
CREATE TABLE conversations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,             -- 'user' | 'assistant' | 'tool_call' | 'tool_result'
    content TEXT NOT NULL,
    tokens_used INTEGER,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- trust_ledger: approved domains for web_request
CREATE TABLE trust_ledger (
    domain TEXT PRIMARY KEY,
    approved_at TEXT NOT NULL,
    approved_by TEXT NOT NULL       -- 'config' | 'user'
);

-- FTS5 indices
CREATE VIRTUAL TABLE memories_fts USING fts5(content, content=memories, content_rowid=id);
CREATE VIRTUAL TABLE conversations_fts USING fts5(content, content=conversations, content_rowid=id);

-- Triggers to keep FTS in sync
-- (standard insert/update/delete triggers)
```

### Search

```rust
pub struct MemoryEngine {
    db: SqlitePool,               // sqlx connection pool
    writer: mpsc::Sender<WriteOp>, // single-writer actor
    embedder: Option<Arc<dyn Embedder>>,  // None if not configured
}

impl MemoryEngine {
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Memory>> {
        let fts_results = self.fts5_search(query, limit * 2).await?;

        if let Some(embedder) = &self.embedder {
            let query_vec = embedder.embed(query).await?;
            let vec_results = self.vector_search(&query_vec, limit * 2).await?;
            Ok(rrf_merge(fts_results, vec_results, limit))
        } else {
            Ok(fts_results.into_iter().take(limit).collect())
        }
    }
}
```

### Embeddings

Optional. Configured via `[models.roles] embedding = "ollama/nomic-embed-text"`.

When configured:
- New memories get embedding populated on insert
- Search uses RRF merge of FTS5 + vector similarity
- Vector search via sqlite-vec extension (single .db file, no second database)

When not configured:
- embedding column stays NULL
- Search uses FTS5 only
- Everything else works identically

### Embedder Trait

```rust
#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
    fn dimensions(&self) -> usize;
}

pub struct OllamaEmbedder { model: String, client: reqwest::Client }
// Calls POST /api/embed with model + input
```

### Write Actor

All writes go through a single-writer actor (mpsc channel). Prevents
SQLite write contention. Reads are concurrent (multiple connections).

```rust
enum WriteOp {
    SaveMemory(Memory),
    SaveConversation(ConversationEntry),
    UpdateMemoryStatus { id: i64, status: String },
    TrustDomain { domain: String },
}
```

---

## Privacy Boundary

```
INSIDE THE BOUNDARY (agent can do anything):
  - Write any code
  - Create/modify/delete tools
  - Install Python packages (pip)
  - Spin up Docker services (docker_manage)
  - Edit its own config (agent.toml)
  - Read/write workspace files
  - Access all memories
  - Reach allowed domains from sandbox scripts

CROSSING THE BOUNDARY (controlled by egress proxy + policy):
  - Sandbox HTTP(S): routed through egress proxy, domain allowlist enforced
  - web_fetch: GET, SSRF filtered, text or file download               → public internet
  - web_request: POST, domain allowlisted, approval for new domains     → allowed domains
  - browser: dedicated Chrome via pipe (host-side, no open port), domain policy gate → approved domains
  - docker_manage: pull images (approval per new image)                 → Docker Hub
  - LLM API: conversation context                                      → model provider
  - Telegram: responses                                                 → user

CANNOT CROSS:
  - memory.db contents (not mounted in sandbox)
  - .env secrets (not in sandbox env)
  - Host filesystem (only workspace + scripts visible from sandbox)
  - Domains not in allowlist (blocked by proxy)
  - Unmanaged Docker containers (only wintermute-labeled)
```

### Egress Control

| Tool | Method | Body | Domain check | Approval | Rate limit |
|------|--------|------|-------------|----------|------------|
| web_fetch | GET only | Never | SSRF filter | No | 30/min |
| web_fetch (save_to) | GET, saves to /workspace | Binary ok | SSRF filter | Files >50MB | 30/min |
| web_request | POST/PUT/PATCH/DELETE | Allowed | Allowlist + SSRF | New domains | 10/min |
| browser | Full HTTP (GET/POST/etc) | Via page interaction | Policy gate (pipe, no proxy) | New domains | 60 actions/min |

### web_fetch Details

Two modes:

**Text mode** (default): returns response body as text. For APIs, HTML,
JSON. Truncated at 100KB.

```json
{ "url": "https://api.example.com/data" }
→ returns: { "status": 200, "body": "...", "content_type": "application/json" }
```

**File mode** (with save_to): downloads response body to /workspace path.
Supports binary. For downloading packages, models, archives, images.

```json
{ "url": "https://example.com/model.bin", "save_to": "/workspace/model.bin" }
→ returns: { "status": 200, "path": "/workspace/model.bin", "size_bytes": 1048576 }
```

Constraints:
- save_to must be under /workspace/ (SSRF + path traversal filtered)
- Files >50MB require user approval (prevents disk filling)
- Max file size: 500MB (configurable in config.toml)
- Runs on the HOST (has network), saves to /workspace (mounted in sandbox)

SSRF filter: resolve DNS → check all IPs (v4 + v6) → reject private
ranges, loopback, link-local, CGNAT, ULA, mapped v4-in-v6.
Manual redirect following with per-hop IP checks. no_proxy() on client.

### Redactor

Single chokepoint. ALL tool output passes through before returning
to the LLM or being stored.

Two layers:
1. Exact match: known secrets loaded from .env
2. Pattern match: sk-ant-*, sk-* (32+), ghp_*, glpat-*, xoxb-*, etc.

### Input Credential Guard

Runs on inbound Telegram messages BEFORE they enter the pipeline.

- Message IS mostly a credential (>50% matches pattern) → block,
  tell user to add to .env
- Credential pattern in longer message → redact before processing

---

## Approval Flow (Non-Blocking)

When a tool call needs approval, the agent doesn't wait.

```rust
match policy_gate.check(&tool_call) {
    PolicyDecision::Allow => {
        let result = execute(tool_call).await;
        ToolResult::success(result)
    }
    PolicyDecision::RequireApproval => {
        let approval_id = approval_manager.request(&tool_call).await;
        // Send Telegram inline keyboard: [✅ Approve] [❌ Deny]
        // Callback data: "a:{8-char-id}" (10 bytes, fits 64-byte limit)
        ToolResult::pending(
            "Waiting for your approval to proceed. I'll continue with other work."
        )
    }
    PolicyDecision::Deny(reason) => {
        ToolResult::error(&reason)
    }
}
```

The LLM receives "waiting for approval" as the tool result. It can:
- Continue with other tool calls
- Tell the user it's waiting
- Work on a different part of the task

When approval arrives (or is denied), it's delivered to the session
as a new event. The agent picks it up on the next turn.

Approval state: 8-char base62 random ID, server-side HashMap, 5-minute
expiry, single-use (deleted after processing), user_id validated.

---

## Scripts & Git

/scripts/ is a git repository. Every change is a commit.

```
wintermute init → git init /scripts/, initial commit

Agent creates tool → git add + commit "create tool: news_digest"
Agent updates tool → git add + commit "update tool: news_digest"
Agent deletes tool → git rm + commit "delete tool: news_digest"
Agent modifies agent.toml → git add + commit "update config: add scheduled task"
Agent writes requirements.txt → git add + commit "add dependency: pandas"
```

Benefits:
- Granular rollback: `git revert HEAD` undoes last change
- History: `git log --oneline` shows what the agent has done
- Diff: see exactly what changed between versions
- Supervisor (v1.1) can inspect and revert programmatically
- Backup simplified: `git bundle create` + `sqlite3 .backup`

The agent sees the git repo but doesn't need to use git directly.
create_tool handles commits. The user can use git commands if they want.

---

## Scheduled Tasks

Defined in agent.toml. Executed by heartbeat.

```toml
[[scheduled_tasks]]
name = "news_digest"
cron = "0 8 * * *"          # cron expression
tool = "news_digest"         # dynamic tool to invoke
budget_tokens = 50000        # per-execution budget
notify = true                # send result to user via Telegram
enabled = true
```

Each execution:
1. Heartbeat fires at cron time
2. Creates a new agent session with limited context:
   - System prompt + task description + relevant memories
   - Own budget (budget_tokens from task config)
3. Invokes the specified tool (or runs a command)
4. If notify=true, sends result to user via Telegram
5. Session cleaned up

Built-in tasks (not agent-removable):
- `daily_backup`: git bundle + sqlite backup. Default 3am.
- `weekly_digest`: consolidate memories → update USER.md, archive stale
  memories, flag contradictions. Default Sunday 4am.

### Health File

Heartbeat writes ~/.wintermute/health.json every cycle. This is the
primary interface for the Flatline supervisor (v1.1) but also useful
for manual monitoring and the /status command.

```json
{
  "status": "running",
  "uptime_secs": 86400,
  "last_heartbeat": "2026-02-19T14:30:00Z",
  "executor": "docker",
  "container_healthy": true,
  "active_sessions": 1,
  "memory_db_size_mb": 12,
  "scripts_count": 23,
  "dynamic_tools_count": 18,
  "budget_today": { "used": 120000, "limit": 5000000 },
  "last_error": null
}
```

### Structured Logging

All logs are structured JSON (.jsonl) for both human debugging and
future Flatline consumption:

```json
{"ts":"...","level":"info","event":"tool_call","tool":"news_digest","duration_ms":1200,"success":true,"session":"abc123"}
{"ts":"...","level":"error","event":"tool_call","tool":"deploy_check","error":"timeout","session":"abc123"}
```

Event types: `tool_call`, `llm_call`, `approval`, `budget`, `session`,
`heartbeat`, `backup`, `tool_created`, `tool_updated`.

---

## Self-Knowledge

The agent must know what it is. Not guess, not infer — KNOW.

OpenClaw solves this with SKILL.md files per skill and a cached skills
snapshot. But Wintermute's problem is broader: the agent doesn't just
need to know its tools, it needs to know its own architecture, its
memory system, its limitations, and how to help the user set things up.

### System Identity Document (SID)

A markdown file the agent reads at startup. Loaded into every conversation
as part of the system prompt. This is the agent's self-knowledge.

Location: ~/.wintermute/IDENTITY.md (generated by `wintermute init`,
updated on config changes, agent can read but not modify)

### User Profile (USER.md)

A curated profile of the user, always loaded into context alongside
the SID. Under 500 tokens. Updated weekly by the built-in `digest`
task.

Location: ~/.wintermute/USER.md (generated by digest, agent can
read but not directly modify — digest curates it from memories)

```markdown
# User Profile

## Identity
- Name: Igor
- Timezone: UTC+8
- Languages: English, Russian, Ukrainian

## Work
- Blockchain infrastructure engineer (Kaspa ecosystem)
- Manages a dev team
- Deep Rust expertise, production deployment on OVH
- Currently building: IGRA L1→L2 pipeline

## Preferences
- Direct communication, no fluff
- Wants code solutions, not explanations
- Prefers local-first tools
- Surfing trips: Bali, Sri Lanka, Mauritius

## Active Projects
- Wintermute (this agent)
- IGRA pipeline deployment
- Team performance reviews

## Communication Style
- Short messages, often voice
- Expects proactive suggestions
- Appreciates when agent pushes back on bad ideas
```

This is NOT the memories table. The memories table has hundreds of
entries. USER.md is a ~30-line distillation — the essential context
the agent needs in EVERY conversation. Think of it as the difference
between a filing cabinet (memories) and a sticky note on your monitor
(USER.md).

### Memory Consolidation (weekly digest)

The built-in `digest` task runs weekly (Sunday 4am by default). It:

1. Reads all active memories (facts, procedures, episodes)
2. Calls the observer model (cheap/local) with a consolidation prompt:
   - Distill user facts into updated USER.md (~30 lines, ~500 tokens)
   - Flag contradictory memories for review
   - Archive stale memories (not referenced in 60+ days, no linked tools)
   - Surface memories that should be promoted to procedures or tools
3. Writes updated USER.md
4. Git commits the change: "digest: update user profile"

The consolidation prompt:

```
Given these memories about the user, produce an updated USER.md profile.
Rules:
- Max 30 lines, ~500 tokens
- Sections: Identity, Work, Preferences, Active Projects, Communication Style
- Only include information that's useful in EVERY conversation
- Drop stale/one-time information
- If two memories contradict, keep the newer one and flag for review
Current USER.md: {current contents}
Memories since last digest: {new memories}
All active memories: {all memories, summarized}
```

The USER.md is loaded into every conversation right after the SID.
The agent always knows who it's talking to without searching.

Contents:

```markdown
# {personality.name}

You are {personality.name}, a self-coding AI agent running on {hostname}.

## Your Architecture
- Core: Rust binary running on the HOST with full network access
- Executor: {docker|direct} mode
  {if docker: "Your code runs in a sandboxed Docker container.
   The sandbox HAS network via an egress proxy. Your scripts can pip install,
   requests.get(), curl — anything HTTP(S). BUT only allowed domains work.
   Unknown domains are blocked by the proxy (HTTP 403).
   Service containers (Ollama, Postgres, etc.) are on a shared Docker
   network — your scripts can reach them directly (e.g., localhost:11434)."}
  {if direct: "Your code runs directly on the host. Full network access.
   Be careful with destructive commands."}

## Topology (important — read this)
```
HOST (has network, has Docker)
  ├── wintermute binary (Rust) ← your core, runs HERE
  │   ├── web_fetch / web_request ← reach the internet
  │   ├── browser ← launches + controls dedicated Chrome via pipe
  │   ├── docker_manage ← creates/manages Docker containers
  │   ├── model router ← talks to Ollama/Anthropic
  │   └── memory engine ← reads/writes memory.db
  │
  ├── Egress proxy ← ALL sandbox HTTP traffic goes through this
  │   └── Enforces domain allowlist from config.toml
  │
  ├── Docker sandbox ← your scripts run HERE
  │   ├── /workspace (shared with host)
  │   ├── /scripts (shared with host)
  │   ├── HAS network, but only through the egress proxy
  │   ├── pip install, requests.get(), curl — all work for allowed domains
  │   ├── Unknown domains → blocked by proxy (HTTP 403)
  │   └── Cannot access Docker socket or host filesystem outside mounts
  │
  └── Service containers (agent-managed, e.g., Ollama, Postgres)
      ├── Created by docker_manage on demand
      ├── On a shared Docker network with the sandbox
      └── Agent's scripts can reach them (e.g., localhost:11434 for Ollama)
```
When you run execute_command, it runs INSIDE the sandbox (has proxy-controlled network).
When you call web_fetch/web_request/browser/docker_manage, they run OUTSIDE (on the host).
The browser launches a dedicated Chrome instance via pipe transport — no open port, no proxy.
Need a service like Ollama or a database? Use docker_manage to spin it up.

## What You CAN Install
- Python packages: `pip install --user <package>` — the sandbox has
  network through the egress proxy. Package registries are always allowed.
  Always add installed packages to /scripts/requirements.txt.
- Docker services: use docker_manage to pull images and run containers.
  Need Ollama? `docker_manage(action="run", image="ollama/ollama")`.
  Need a database? Same pattern. Service containers persist across restarts.
  {if direct mode: "pip install and Docker both work normally."}

## What You CANNOT Do
- Access domains not in the allowlist from the sandbox (proxy blocks them).
  To add a domain: request it, user approves, it gets added.
- Access the host filesystem outside of /workspace and /scripts.
- Run privileged operations (no sudo, no capabilities).
- Manage Docker containers not created by you (only wintermute-labeled).

## When You Need Something That Isn't Available
1. Check if it's available as a Docker image → use docker_manage to run it
2. Check if it's a Python package → pip install it
3. If it requires native host installation (e.g., GPU drivers, Docker
   itself, system libraries): tell the user what you need, give them
   the exact install command, and wait for confirmation

## Your Memory
- Storage: SQLite + FTS5 {if embeddings: "+ vector search via {embedding_model}"}
  {if no embeddings: "Vector search not configured. Memory uses keyword search only.
   You can enable it by asking the user to configure an embedding model in config.toml."}
- {n} active memories ({n} facts, {n} procedures, {n} episodes, {n} skills)

## Your Tools
### Core tools (always available):
- execute_command: Run shell commands in {docker|direct} sandbox
- create_tool: Create reusable tools in /scripts/ (Python + JSON schema)
- web_fetch: HTTP GET (no body, 30/min)
- web_request: HTTP POST/PUT/etc (domain allowlisted, approval for new domains)
- browser: {managed (dedicated Chrome, pipe transport)|sidecar (headless Docker)|not available}
- memory_search: Search your memories ({keyword|keyword + vector} search)
- memory_save: Save facts, procedures, episodes, skills
- send_telegram: Send messages + files to the user

### Your custom tools ({n} total):
{for each dynamic tool: "- {name}: {description} (last used: {date}, success rate: {n}%)"}

## Your Memory
- {n} active memories ({n} facts, {n} procedures, {n} episodes, {n} skills)
- {n} pending memories awaiting promotion
- Search: {keyword only | keyword + vector similarity}
  {if keyword only: "You can enable vector search by configuring an embedding model.
   Ask the user: 'Want me to help set up semantic search? You'll need Ollama running
   with nomic-embed-text.'"}

## Your Model
- Current model: {model_id} via {provider}
- Observer model: {observer_model_id|"same as main"}
- Embedding model: {embedding_model|"not configured"}

## Privacy Boundary
- Your sandbox has network, but ALL traffic goes through an egress proxy.
  Only domains in the allowlist (config.toml) are reachable.
- You cannot see ~/.wintermute/.env or memory.db from inside the sandbox.
- Everything in /scripts/ is git-versioned. Every change you make is a commit.
- Docker images require user approval on first pull.
- You can only manage Docker containers you created (wintermute-labeled).

## What You Can Modify About Yourself
You can evolve. This is by design.

**Your personality (agent.toml → [personality].soul):**
You can rewrite your own soul. If the user asks you to be more concise,
more opinionated, funnier, more formal — update your soul. If you notice
your communication style doesn't match what the user wants, propose a
change. The soul is loaded fresh every conversation, so changes take
effect immediately.

**Your tools (/scripts/):** create_tool makes new capabilities permanent.

**Your memory:** memory_save accumulates knowledge over time.

**Your scheduled tasks (agent.toml → [[scheduled_tasks]]):** add, modify,
or remove automated tasks.

**Your services (agent.toml → [[services]]):** spin up or tear down
Docker services you depend on.

## What You CANNOT Modify
**config.toml** — security policy, credentials, domain allowlist, budget
limits. These are the user's constraints on you. Respect them.

**IDENTITY.md** — your self-knowledge document. Generated by the system,
not by you. You read it, you don't write it.

**USER.md** — the user's profile. Updated by the weekly digest, not by
you directly. You contribute to it by saving memories.

**Your core binary** — the Rust code that runs you. You extend yourself
through tools and config, not by recompiling.

## Self-Modification Protocol
When modifying your own soul or config:
1. Tell the user what you want to change and why
2. Show the before/after
3. Wait for approval (this is a personality change, not a tool call)
4. Apply via execute_command editing agent.toml
5. Git commit: "evolve: {what changed}"
6. The change takes effect on the next conversation

## What You Can Help Set Up
- Spin up services: docker_manage to run Ollama, databases, Redis, etc.
- Enable vector search: docker_manage to run Ollama + pull embedding model
- Add new domains: request approval, they get added to the allowlist
- Install packages: pip install directly in the sandbox
- Create scheduled tasks: edit agent.toml to add cron-triggered tools
- Configure model routing: explain per-role/per-skill model options

## Handling Non-Text Messages
When you receive a voice message, photo, or document you can't process:
- Tell the user what you received and where the file is
- Offer to build a tool to handle it using create_tool
- For voice: transcribe via whisper (Ollama model, pip package, or API)
- For images: describe via multimodal model or OCR via tesseract
- For documents: extract text via Python libraries (pypdf, docx, etc.)
Once you create the tool, you'll handle that media type automatically.

## How to Create Tools
When you solve a repeatable task:
1. Write the script, test it in /workspace
2. Use create_tool to save it as a reusable tool
3. The tool appears in your tool list immediately
4. Next time the same task comes up, call the tool directly

## Scheduled Tasks
{for each task: "- {name}: {cron} → {tool|command} (last run: {status})"}

## Current State
- Uptime: {uptime}
- Budget today: {used}/{limit} tokens ({percent}%)
- Active sessions: {n}
- Last backup: {time}
```

The SID is **generated**, not hand-written. `wintermute init` creates it.
The heartbeat regenerates it periodically (every 5 minutes) so it reflects
current state — tool count, memory stats, budget usage, executor status.

### Why This Matters

Without the SID, the agent says things like:
> "The system appears to support embeddings"
> "I don't have direct control over it"
> "The vector similarity seems to happen automatically"

With the SID, the agent says:
> "My memory uses FTS5 keyword search. Vector search isn't configured yet.
> Want me to help you set it up? You'll need Ollama running with
> nomic-embed-text."

The agent goes from confused to competent about itself.

### Onboarding Conversation

On first launch (no memories, no tools), the SID includes:

```markdown
## First Run
This is a fresh install. You have no custom tools or memories yet.
Your USER.md profile is empty — learn about the user in this conversation.

Start by introducing yourself and learning about them:
- Who are they? (name, work, projects)
- What do they want automated? (goals, not just tasks)
- How do they like to communicate? (detailed vs terse, push back vs just do it)
- What does "a good month from now" look like with your help?

Save what you learn with memory_save. The weekly digest will curate
it into their profile. But for now, save generously — you can always
prune later.

After learning about them, suggest building your first tool together.
Pick something they'd use daily.
```

This replaces OpenClaw's onboarding wizard with a conversational
equivalent — the agent conducts its own onboarding via Telegram.

---

## Budget Management

### The Problem

The agent hit 569K of 500K tokens and got cut off mid-conversation.
No warning. No graceful degradation. No recovery. The user gets a raw
error message.

### Budget Awareness

Budget status is in the SID (refreshed periodically). But the agent
also needs real-time awareness during a conversation.

The context assembler injects a budget warning when usage crosses
thresholds:

```
At 70%: [System: Budget at 70%. Consider wrapping up or summarizing.]
At 85%: [System: Budget at 85%. Finish current task. Avoid new tool calls.]
At 95%: [System: Budget at 95%. Send final response now. Next message will fail.]
```

These are injected as system messages in the conversation, not visible
to the user. The agent sees them and can act accordingly.

### Graceful Exhaustion

When budget is exceeded, the agent should NOT crash silently. Instead:

```rust
match budget_tracker.check_budget() {
    BudgetStatus::Ok => { /* proceed normally */ }
    BudgetStatus::Warning(percent) => {
        // Inject warning into context, continue
        context.add_system_note(&format!(
            "Budget at {}%. Wrap up current task.", percent
        ));
    }
    BudgetStatus::Exhausted => {
        // Don't call LLM. Send a direct message instead.
        telegram.send(
            "⚠️ Daily token budget reached. I'll be back tomorrow, \
             or you can adjust the limit in config.toml under \
             [budget].max_tokens_per_day."
        ).await;
        session.suspend().await;
    }
}
```

The agent tells the user what happened AND how to fix it. Not a raw
error string.

### Session Budget vs Daily Budget

Two separate limits, both enforced:

- **Per-session**: protects against runaway single conversations.
  When exceeded: "This conversation is getting long. I can continue
  in a new session, or you can increase the session limit."

- **Per-day**: protects against cost overrun across all sessions.
  When exceeded: "Daily budget reached. Back tomorrow."

### Context Compaction for Long Conversations

For conversations that naturally need many tokens (like this architecture
review), the agent should compact context before hitting the limit:

```
At 60% of session budget:
  1. Summarize older messages in the conversation
  2. Replace detailed tool results with summaries
  3. Keep recent messages + system prompt + tool definitions intact
  4. Continue with compressed context
```

This is how Claude.ai and OpenClaw handle long conversations —
auto-compaction preserves continuity while freeing token space.

Implementation: when context exceeds 60% of session budget, the agent
calls the LLM with a compaction prompt:

```
Summarize this conversation so far in a way that preserves:
- All decisions made
- All action items
- Current task state
- Key facts mentioned
Keep it under {target_tokens} tokens.
```

The summary replaces the old messages. The conversation continues
with the summary as context.

### Budget in /status Command

```
/status shows:
  Session: 45,000 / 500,000 tokens (9%)
  Today:   320,000 / 5,000,000 tokens (6.4%)
  Est. cost today: $0.48
```

---

## Telegram Interface

HTML parse mode only. Escape only `< > &`.

### Commands

```
/status              Health, sandbox, memory stats, active tasks
/budget              Token usage today, limits, estimated cost
/memory              Overview of facts + procedures
/memory pending      Staged extractions awaiting promotion
/memory undo         Reverse last observer batch
/tools               List dynamic tools with usage stats
/tools {name}        Show tool details + recent invocations
/sandbox             Container status (or "direct mode" if no Docker)
/reset               Recreate sandbox (reinstalls requirements.txt)
/backup              Trigger immediate backup
/help                List commands
```

### File Support

send_telegram supports file attachments:
```json
{
  "text": "Here's your report",
  "file": "/workspace/report.pdf"
}
```

The agent creates files in /workspace, sends via Telegram.

### Voice Messages & Other Media

The core doesn't transcribe voice, parse images, or extract PDFs.
Those are capabilities the agent builds for itself.

The Telegram adapter handles non-text messages by downloading the file
and passing a description to the agent:

```
User sends voice →
Agent receives: "[Voice message: /workspace/inbox/voice_20260223.ogg, 12s]"

User sends photo →
Agent receives: "[Photo: /workspace/inbox/photo_20260223.jpg]"

User sends document →
Agent receives: "[Document: /workspace/inbox/report.pdf]"
```

First time the agent gets a voice message, it has no transcription tool.
The SID guides it to offer building one:

1. "I got your voice message but can't transcribe it yet.
   Want me to set up speech-to-text?"
2. If yes: `create_tool` → `transcribe_audio` (whisper via Ollama,
   or a pip package, or an API — agent figures out what's available)
3. Transcribe the message using the new tool
4. From now on: agent checks for `transcribe_audio` tool when voice
   arrives and uses it automatically

This is the self-coding flywheel. New input type → agent builds a
capability → handles it forever. Same pattern applies to images
(OCR, multimodal description), PDFs (text extraction), etc.

---

## Observer (Staged Learning)

Extracts facts and procedures from conversations. Nothing goes directly
into active memory.

### Pipeline

1. Session goes idle (no messages for 2 minutes)
2. Observer runs extraction using observer model (cheap/local)
3. Extracted items enter `pending` status
4. Promotion based on config:
   - `auto`: promote after N consistent extractions (default 3)
   - `suggest`: send Telegram suggestion, user approves
   - `off`: no extraction, only explicit memory_save

### Safeguards

- Contradictions: if new extraction conflicts with existing memory,
  flag for user review instead of auto-promoting
- Corrections: if user says "actually X", apply immediately (not staged)
- Rollback: `/memory undo` reverses last observer batch

---

## Security Posture (Honest)

### What we enforce

- Sandbox network controlled by egress proxy: only allowed domains reachable
- All sandbox commands run with no capabilities, read-only rootfs (Docker mode)
  or in restricted directory (direct mode)
- No secrets in sandbox environment
- POST/PUT/DELETE to unknown domains requires explicit user approval
- Docker image pulls require user approval (first use of each image)
- Agent can only manage its own Docker containers (wintermute-labeled)
- Budget limits with atomic counters, checked before every LLM call
- Inbound credentials blocked/redacted before pipeline
- Config split: agent cannot modify security policy

### What we mitigate best-effort

- Secret redaction (known values + patterns; novel formats may leak)
- SSRF filtering (comprehensive; exotic attacks may bypass)
- Command classification (LLM can wrap destructive commands in scripts)

### Honest limitations

- Docker: strong isolation but shares host kernel. Kernel CVE = host
  compromise. Configure gVisor/Kata for stronger boundary.
- Direct mode: no egress proxy. Privacy depends on agent behavior.
- Egress proxy: domain-level control. Cannot inspect encrypted content.
  A malicious script could exfiltrate data to an allowed domain.
- Redaction: pattern-based. Will miss novel secret formats.
- The agent modifies its own tools. A corrupted tool runs until
  someone notices. Git history enables rollback.
- The agent can spin up Docker containers. A malicious prompt could
  create resource-intensive containers. PID/memory limits and the
  wintermute label policy mitigate this.

### Worst-case scenarios

| Scenario | Impact | Recovery |
|----------|--------|----------|
| Prompt injection | /workspace + /scripts deletable. POST blocked by domain allowlist. | git revert + sqlite backup |
| Container escape (kernel CVE) | Host compromised | gVisor/Kata. Residual risk. |
| Infinite loop | Tool call cap (20/turn) + daily token limit | Budget auto-stops |
| Secret in tool output | Redacted best-effort. No secrets in sandbox. | Rotate the secret |
| Bad tool created | Runs until noticed | git revert, /tools inspect |
| Observer hallucination | Sits in pending. Needs N consistent extractions. | /memory pending reject |

---

## Project Structure

```
wintermute/
├── Cargo.toml
├── Dockerfile.sandbox
├── config.example.toml
├── migrations/
│   └── 001_schema.sql
├── src/
│   ├── main.rs                    # CLI + startup
│   ├── config.rs                  # config.toml + agent.toml loading
│   ├── credentials.rs             # .env loading
│   │
│   ├── providers/
│   │   ├── mod.rs                 # LlmProvider trait
│   │   ├── anthropic.rs           # Anthropic API + native tool calling
│   │   ├── openai.rs              # OpenAI-compatible API + native tool calling
│   │   ├── ollama.rs              # Ollama API + native tool calling
│   │   └── router.rs              # ModelRouter (default → role → skill)
│   │
│   ├── executor/
│   │   ├── mod.rs                 # Executor trait
│   │   ├── docker.rs              # DockerExecutor (bollard, warm container)
│   │   ├── direct.rs              # DirectExecutor (host, restricted dir)
│   │   ├── egress.rs              # Egress proxy (Squid config, domain allowlist)
│   │   └── redactor.rs            # Secret pattern redaction
│   │
│   ├── tools/
│   │   ├── mod.rs                 # Tool routing (core + dynamic)
│   │   ├── core.rs                # 9 core tool implementations
│   │   ├── registry.rs            # Dynamic tool registry + hot-reload
│   │   ├── create_tool.rs         # create_tool implementation + git commit
│   │   ├── browser.rs             # Browser tool: chromiumoxide pipe + sidecar fallback
│   │   ├── browser_sidecar.rs     # Sidecar lifecycle (bollard) + HTTP bridge client
│   │   └── docker.rs              # docker_manage (bollard, label policy)
│   │
│   ├── agent/
│   │   ├── mod.rs                 # Session router (per-session tasks)
│   │   ├── loop.rs                # Agent loop (assemble → LLM → route → execute)
│   │   ├── context.rs             # Context assembly + trimming + compaction
│   │   ├── identity.rs            # SID generator (IDENTITY.md from config + state)
│   │   ├── policy.rs              # Policy gate + egress rules
│   │   ├── approval.rs            # Non-blocking approval (short-ID callbacks)
│   │   └── budget.rs              # Token/cost budget (atomic, warnings, graceful exhaustion)
│   │
│   ├── memory/
│   │   ├── mod.rs                 # MemoryEngine
│   │   ├── writer.rs              # Write actor (mpsc)
│   │   ├── search.rs              # FTS5 + optional vector (sqlite-vec)
│   │   └── embedder.rs            # Embedder trait + OllamaEmbedder
│   │
│   ├── telegram/
│   │   ├── mod.rs                 # Adapter (teloxide)
│   │   ├── input_guard.rs         # Credential detection + redaction
│   │   ├── media.rs               # Non-text messages: download file, pass description
│   │   ├── ui.rs                  # HTML formatting, keyboards, file sending
│   │   └── commands.rs            # /status, /budget, /memory, /tools, etc.
│   │
│   ├── observer/
│   │   ├── mod.rs                 # Observer pipeline
│   │   ├── extractor.rs           # LLM extraction (observer model)
│   │   └── staging.rs             # Pending → active promotion
│   │
│   └── heartbeat/
│       ├── mod.rs                 # Tick loop
│       ├── scheduler.rs           # Cron evaluation + task dispatch
│       ├── backup.rs              # git bundle + sqlite backup
│       ├── digest.rs              # Weekly memory consolidation → USER.md
│       └── health.rs              # Write health.json + self-checks
│
└── tests/
    ├── tool_registry_test.rs
    ├── policy_test.rs
    ├── memory_test.rs
    └── approval_test.rs
```

---

## Dockerfile.sandbox

Minimal. The agent installs what it needs.

```dockerfile
FROM python:3.12-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    curl git jq bc timeout coreutils \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash wintermute
USER wintermute
WORKDIR /workspace

# Agent installs packages at runtime via pip install --user
# Persists in ~/.local until container reset
# requirements.txt auto-installed on container creation
```

On container creation / reset:
```bash
if [ -f /scripts/requirements.txt ]; then
    pip install --user -r /scripts/requirements.txt
fi
```

---

## Build & Run

```bash
# Build
cargo build --release

# First time setup
./wintermute init
# Creates ~/.wintermute/, config.toml, agent.toml, .env template
# Initializes git repo in scripts/
# Builds Docker image (if Docker available)
# Runs migrations on memory.db

# Start
./wintermute start

# Operations
./wintermute status              # Health check
./wintermute reset               # Recreate sandbox, reinstall deps
./wintermute backup              # Immediate backup
./wintermute backup list         # Show available backups
./wintermute backup restore N    # Restore specific backup
```

---

## Implementation Plan

### Phase 1: Foundation (weeks 1-3)

**Task 1: Scaffold**
Files: Cargo.toml, main.rs, config.rs, credentials.rs
- CLI skeleton (clap): init, start, status, reset, backup
- Config loading: config.toml (human) + agent.toml (agent)
- Credential loading from .env
- Logging setup (tracing, structured JSON)

**Task 2: Providers + Router**
Files: providers/*
- LlmProvider trait
- AnthropicProvider (native tool calling, streaming)
- OpenAiProvider (OpenAI-compatible: OpenAI, DeepSeek, Groq, Together, etc.)
- OllamaProvider (native tool calling via /api/chat)
- ModelRouter (default → role → skill resolution)
- Provider instantiation from config strings ("anthropic/claude-sonnet")

**Task 3: Executor**
Files: executor/*, tools/docker.rs, Dockerfile.sandbox
- Executor trait
- DockerExecutor: bollard, warm container, egress proxy (Squid), all hardening
  GNU timeout wrapping, health_check, requirements.txt install on reset
- Egress proxy: generate Squid config from config.toml allowlist,
  start as sidecar, sandbox routes HTTP(S) through it
- DirectExecutor: subprocess in restricted dir, no proxy, warnings logged
- Auto-detection at startup
- Redactor: exact match + regex patterns, single chokepoint
- docker_manage tool: run/stop/pull/logs/exec, wintermute label policy,
  approval for new image pulls, service persistence in agent.toml

### Phase 2: Core Loop (weeks 4-6)

**Task 4: Memory**
Files: memory/*, migrations/
- SQLite schema + FTS5 + triggers
- Write actor (mpsc channel)
- FTS5 search
- Optional: sqlite-vec + OllamaEmbedder + RRF merge
- Embedder trait + OllamaEmbedder implementation

**Task 5: Telegram**
Files: telegram/*
- teloxide adapter, HTML formatting
- Input credential guard (block + redact)
- Non-text media: download voice/photo/document to /workspace/inbox/,
  pass description to agent ("[Voice message: /workspace/inbox/voice.ogg, 12s]")
- File sending support
- Inline keyboard support (for approvals)
- Message routing to agent sessions

**Task 6: Agent Loop + Tools**
Files: agent/*, tools/*
- SID generator (reads config + state → IDENTITY.md, refreshed by heartbeat)
- Session router (per-session Tokio tasks, try_send)
- Agent loop: context assemble → LLM call → tool routing → execute → observe
- Context assembler with trimming + retry + compaction at 60% session budget
- Budget tracker: atomic counters, per-session + daily, warnings at 70/85/95%
- Budget exhaustion: graceful message to user, session suspend (not crash)
- Policy gate + egress rules
- Non-blocking approval manager (short-ID callbacks)
- 9 core tool implementations
- Dynamic tool registry (watch /scripts/*.json, hot-reload)
- create_tool implementation (write files + git commit)
- Dynamic tool execution (JSON stdin → sandbox → JSON stdout)
- Dynamic tool selection (top N by relevance/recency)
- Browser: chromiumoxide pipe transport (managed Chrome, dedicated profile),
  Docker sidecar fallback (headless, Playwright + Flask). On-demand lifecycle
  with idle timeout. Auto-detect mode at startup. Skip if neither available.

### Phase 3: Intelligence (weeks 7-8)

**Task 7: Observer**
Files: observer/*
- Session idle detection
- LLM extraction (uses observer model)
- Staging: pending → active promotion
- Contradiction detection
- /memory pending, /memory undo

**Task 8: Heartbeat + Operations**
Files: heartbeat/*, telegram/commands.rs
- Tick loop + cron evaluation
- Scheduled task dispatch (own session + budget)
- Backup: git bundle for /scripts + sqlite .backup for memory.db
- Digest: weekly memory consolidation → USER.md (calls observer model,
  curates profile, archives stale memories, flags contradictions)
- Health self-checks + structured logging
- All Telegram commands

### v1.1 (post-launch)

- **Flatline** — supervisor process. See wintermute-flatline.md.
  Separate binary, reads logs + health.json + git history,
  diagnoses failures, quarantines bad tools, restarts crashed process,
  proposes fixes via Telegram. Own LLM budget (cheap model).
- OS-native sandboxing (bubblewrap on Linux, sandbox-exec on macOS)
- LanceDB migration if sqlite-vec insufficient
- VOYAGER-style skill verification (LLM critic evaluates tool output)
- Per-skill model overrides
- MCP server/client support
- Web UI (optional, alongside Telegram)

---

## Dependency Summary

```toml
[dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"

# Database
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite"] }

# HTTP
reqwest = { version = "0.12", features = ["json", "stream"] }

# Telegram
teloxide = { version = "0.13", features = ["macros"] }

# Docker
bollard = "0.18"

# Browser (CDP over pipe)
chromiumoxide = { version = "0.7", features = ["tokio-runtime"] }

# CLI
clap = { version = "4", features = ["derive"] }

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }

# Utilities
chrono = { version = "0.4", features = ["serde"] }
rand = "0.8"
uuid = { version = "1", features = ["v4"] }
regex = "1"
notify = "7"          # filesystem watcher for /scripts hot-reload
cron = "0.12"         # cron expression parsing

# Optional: vector search
# sqlite-vec via sqlx custom extension loading
```

No hmac, sha2, hex (approval uses random IDs, not HMAC).
No rig or ollama-rs (direct HTTP to Ollama /api/chat is simpler).