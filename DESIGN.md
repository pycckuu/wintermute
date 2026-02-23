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

Week 1: 8 built-in tools. Does everything through execute_command.
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
│  │  ├── Message router (per-session, try_send, never blocks)  │   │
│  │  └── File sending support                                  │   │
│  │                                                            │   │
│  │  Agent Loop                                                │   │
│  │  ├── Context Assembler (trim, retry on overflow)           │   │
│  │  ├── Model Router (default → role → skill override)        │   │
│  │  ├── Tool Router                                           │   │
│  │  │   ├── Core Tools (8, built into binary)                 │   │
│  │  │   └── Dynamic Tools (from /scripts/*.json, hot-reload)  │   │
│  │  ├── Policy Gate (approval for destructive + new domains)  │   │
│  │  ├── Approval Manager (non-blocking, short-ID callbacks)   │   │
│  │  ├── Egress Controller (GET open, POST allowlisted)        │   │
│  │  ├── Budget Tracker (atomic counters, per-session + daily) │   │
│  │  └── Redactor (single chokepoint, all tool output)         │   │
│  │                                                            │   │
│  │  Memory Engine                                             │   │
│  │  ├── SQLite + FTS5 (always available)                      │   │
│  │  └── sqlite-vec (when embedding model configured)          │   │
│  │                                                            │   │
│  │  Background                                                │   │
│  │  ├── Observer (staged learning from conversations)         │   │
│  │  ├── Heartbeat (scheduled tasks, health, backup)           │   │
│  │  └── Tool Registry (watches /scripts/, hot-reloads)        │   │
│  │                                                            │   │
│  │  Executor (auto-detected)                                  │   │
│  │  ├── DockerExecutor (preferred: full isolation)            │   │
│  │  └── DirectExecutor (fallback: host, stricter policy)      │   │
│  │                                                            │   │
│  └────────────────────────────────────────────────────────────┘   │
│                                                                   │
│  ┌─ Sandbox (Docker, when available) ────────────────────────┐   │
│  │  Network:     NONE                                         │   │
│  │  Caps:        ALL dropped                                  │   │
│  │  Root FS:     read-only                                    │   │
│  │  User:        wintermute (non-root)                        │   │
│  │  Writable:    /workspace, /scripts, /tmp (tmpfs)           │   │
│  │  NOT mounted: /data, Docker socket, host home              │   │
│  │  Env vars:    NONE                                         │   │
│  │  PID limit:   256   Memory: 2GB   CPU: 2 cores             │   │
│  │  Timeout:     GNU timeout wraps every command               │   │
│  └────────────────────────────────────────────────────────────┘   │
│                                                                   │
│  ~/.wintermute/                                                   │
│  ├── config.toml       (human-owned: security, credentials)      │
│  ├── agent.toml        (agent-owned: personality, tasks)         │
│  ├── .env              (secrets, chmod 600)                      │
│  ├── data/memory.db    (NOT in sandbox)                          │
│  ├── health.json       (written by heartbeat, read by Flatline)    │
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
```

### agent.toml — agent-owned, modifiable via execute_command

```toml
[personality]
name = "Wintermute"
soul = """
You are a personal AI agent. Competent, direct, proactive.
You solve problems by writing code, testing it, and iterating.
When you solve something reusable, save it as a tool with create_tool.
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

### 8 Core Tools (built into binary)

```
execute_command   Run a shell command in the sandbox.
create_tool       Create or update a dynamic tool (/scripts/{name}.py + .json).
web_fetch         HTTP GET. No body. SSRF filtered. 30/min.
web_request       HTTP POST/PUT/PATCH/DELETE. Domain allowlisted. 10/min.
browser           Control a browser. Navigate, click, type, screenshot, extract.
                  Only available on non-headless machines. Host-side (has network).
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

Available on non-headless machines. Auto-detected at startup (checks for
display server / Playwright installation). Headless servers skip it — the
tool simply doesn't appear in the tool list.

### Why it's a core tool, not a dynamic tool

Browser automation runs on the HOST, not in the sandbox. It needs network
access and a display. This puts it in the same category as web_fetch and
web_request — a host-side capability with privacy implications.

Unlike web_fetch (GET only, no interaction) and web_request (single HTTP
call), the browser enables multi-step interaction: navigate, wait, click,
type, extract, screenshot. This is essential for tasks like checking
dashboards, filling forms, scraping JS-rendered content, or monitoring
pages that don't have APIs.

### Implementation: Playwright via subprocess

Playwright (Python) controlled via a long-running subprocess. The Rust
core sends commands over stdin/stdout (JSON protocol), Playwright executes
them in a real browser.

```
Wintermute (Rust)
    │
    │  JSON commands via stdin/stdout
    ▼
browser_bridge.py (long-running Python subprocess)
    │
    │  Playwright API
    ▼
Chromium (or Firefox/WebKit)
```

browser_bridge.py ships with the binary (embedded or in /scripts/_system/).
It's NOT agent-modifiable — it's part of core infrastructure.

### Tool Definition

```json
{
  "name": "browser",
  "description": "Control a browser. Navigate pages, interact with elements, take screenshots, extract content. Only available on machines with a display.",
  "parameters": {
    "action": {
      "type": "string",
      "enum": ["navigate", "click", "type", "screenshot", "extract",
               "wait", "scroll", "evaluate", "close"],
      "description": "Browser action to perform"
    },
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

### Actions

| Action | Description | Returns |
|--------|-------------|---------|
| navigate | Go to URL | Page title, final URL |
| click | Click element by selector | Success/failure |
| type | Type text into element | Success/failure |
| screenshot | Capture viewport or element | File path in /workspace |
| extract | Get text/attribute from selector | Extracted content |
| wait | Wait for selector or network idle | Success/timeout |
| scroll | Scroll page or element | New scroll position |
| evaluate | Run JavaScript in page context | JS return value |
| close | Close browser session | Confirmation |

### Privacy Implications

The browser has FULL network access. It's the most powerful egress
channel in the system. Privacy controls:

1. **Domain policy applies.** browser navigate to unknown domains follows
   the same approval rules as web_request. Pre-approved domains pass,
   unknown domains require user approval.

2. **No silent data extraction.** The agent can see page content (via
   extract/evaluate), but can only send it externally via web_request
   (which is domain-allowlisted). The browser itself doesn't POST form
   data without the agent explicitly choosing to — and that goes through
   policy gate.

3. **Screenshots saved locally.** Screenshots go to /workspace, not
   transmitted anywhere unless the agent explicitly sends them.

4. **Session isolation.** Browser context is ephemeral — no persistent
   cookies, no saved passwords, no history between sessions. Fresh
   context per task unless the agent explicitly manages state.

### Auto-detection

```rust
async fn detect_browser() -> Option<BrowserCapability> {
    // 1. Check if Playwright is installed
    //    python3 -c "import playwright; print('ok')"
    // 2. Check if display is available
    //    Linux: DISPLAY or WAYLAND_DISPLAY env var
    //    macOS: always available (Quartz)
    // 3. Check if browsers are installed
    //    playwright install --dry-run chromium
    // If all pass: return Some(BrowserCapability)
    // If any fail: return None, tool not registered
}
```

On headless servers: browser tool simply doesn't appear in the tool list.
The agent never knows it exists. No error, no degraded mode.

### Rate Limiting

Browser actions: 60/min (generous — interactions are naturally slow).
Navigate to new domain: follows egress policy (approval if unknown).
Screenshot: max 10/min (disk space protection).

### Setup

Not required. On first use, if Playwright isn't installed:
```
pip install playwright && playwright install chromium
```

The agent can do this itself via execute_command when it needs browser
access. Or the user pre-installs during `wintermute init`.

Auto-detected at startup. Two implementations.

```rust
#[async_trait]
pub trait Executor: Send + Sync {
    async fn execute(&self, command: &str, opts: ExecOptions) -> Result<ExecResult>;
    async fn health_check(&self) -> Result<HealthStatus>;
    fn has_network_isolation(&self) -> bool;
    fn scripts_dir(&self) -> &Path;
    fn workspace_dir(&self) -> &Path;
}
```

### DockerExecutor — production, full isolation

Pre-warmed container (always running, use `docker exec`). < 100ms per command.

```
Network:        none
Capabilities:   ALL dropped, none added
Root FS:        read-only
User:           wintermute (non-root)
PID limit:      256
Memory:         configurable (default 2GB)
CPU:            configurable (default 2 cores)
Mounts:         /workspace (rw), /scripts (rw), /tmp (tmpfs 512M)
NOT mounted:    /data, .env, Docker socket, host home
Env vars:       NONE
```

Every command wrapped with GNU timeout inside the container:
```
timeout --signal=TERM --kill-after=5 {secs} bash -c {command}
```

Client-side Tokio timeout as backstop (+10s grace).

Package management: `pip install --user` in warm container. Persists
until container reset. Agent maintains /scripts/requirements.txt.
On `wintermute reset-sandbox`, fresh container runs:
`pip install --user -r /scripts/requirements.txt`

### DirectExecutor — development, macOS, no Docker

Runs commands directly on host in a restricted working directory.
No network isolation. No filesystem isolation beyond directory scoping.

Policy gate compensates:
- web_request: always require approval (no domain auto-trust)
- execute_command: require approval for commands containing `rm -rf`, `sudo`,
  or touching paths outside workspace/scripts
- Higher logging verbosity

The agent is told in its system prompt that it's running without a sandbox
and should be more careful.

### Selection

```rust
let executor: Arc<dyn Executor> = if docker_available().await {
    Arc::new(DockerExecutor::new(&config).await?)
} else {
    tracing::warn!("Docker not available. Running in direct mode (less isolated).");
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
  - Install packages
  - Edit its own config (agent.toml)
  - Read/write workspace files
  - Access all memories

CROSSING THE BOUNDARY (controlled):
  - web_fetch: GET only, no body, SSRF filtered         → public internet
  - web_request: POST, domain allowlisted, approval      → allowed domains
  - browser: full network, domain policy applies          → approved domains
  - LLM API: conversation context                        → model provider
  - Telegram: responses                                   → user
  - pip install: package name only                        → PyPI/npm/apt

CANNOT CROSS:
  - memory.db contents (not mounted in sandbox)
  - .env secrets (not in sandbox env)
  - Host filesystem (only workspace + scripts visible)
  - Bulk data via POST (requires domain approval)
```

### Egress Control

| Tool | Method | Body | Domain check | Approval | Rate limit |
|------|--------|------|-------------|----------|------------|
| web_fetch | GET only | Never | SSRF filter | No | 30/min |
| web_request | POST/PUT/PATCH/DELETE | Allowed | Allowlist + SSRF | New domains | 10/min |
| browser | Full HTTP (GET/POST/etc) | Via page interaction | Allowlist | New domains | 60 actions/min |

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

## System Prompt

Assembled per-turn. Structure:

```
{personality from agent.toml}

## Environment
{executor_type}: Docker sandbox (no network) | Direct (host, be careful)
Working directory: /workspace
Your tools directory: /scripts/ (git-versioned)
Package management: pip install --user <package>

## Your Tools
[core tools always listed]
[top N dynamic tools by relevance/recency]

## Your Capabilities
[contents of /scripts/*.json descriptions — one line each]

## Your Memories
[relevant memories from search, if query context available]

## Current Context
[date, time, user timezone if known]
[any active scheduled tasks]
[pending approvals if any]
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

- All sandbox commands run with no network, no capabilities, read-only rootfs
  (Docker mode) or in restricted directory (direct mode)
- No secrets in sandbox environment
- POST/PUT/DELETE to unknown domains requires explicit user approval
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
- Direct mode: no network isolation. Privacy depends entirely on
  egress control. A bug in egress = data exposure.
- Redaction: pattern-based. Will miss novel secret formats.
- The agent modifies its own tools. A corrupted tool runs until
  someone notices. Git history enables rollback.

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
│   │   ├── ollama.rs              # Ollama API + native tool calling
│   │   └── router.rs              # ModelRouter (default → role → skill)
│   │
│   ├── executor/
│   │   ├── mod.rs                 # Executor trait
│   │   ├── docker.rs              # DockerExecutor (bollard, warm container)
│   │   ├── direct.rs              # DirectExecutor (host, restricted dir)
│   │   └── redactor.rs            # Secret pattern redaction
│   │
│   ├── tools/
│   │   ├── mod.rs                 # Tool routing (core + dynamic)
│   │   ├── core.rs                # 8 core tool implementations
│   │   ├── registry.rs            # Dynamic tool registry + hot-reload
│   │   ├── create_tool.rs         # create_tool implementation + git commit
│   │   └── browser.rs             # Browser bridge (Playwright subprocess)
│   │
│   ├── agent/
│   │   ├── mod.rs                 # Session router (per-session tasks)
│   │   ├── loop.rs                # Agent loop (assemble → LLM → route → execute)
│   │   ├── context.rs             # Context assembly + trimming
│   │   ├── policy.rs              # Policy gate + egress rules
│   │   ├── approval.rs            # Non-blocking approval (short-ID callbacks)
│   │   └── budget.rs              # Token/cost budget (atomic counters)
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
- OllamaProvider (native tool calling via /api/chat)
- ModelRouter (default → role → skill resolution)
- Provider instantiation from config strings ("anthropic/claude-sonnet")

**Task 3: Executor**
Files: executor/*, Dockerfile.sandbox
- Executor trait
- DockerExecutor: bollard, warm container, network:none, all hardening
  GNU timeout wrapping, health_check, requirements.txt install on reset
- DirectExecutor: subprocess in restricted dir, warnings logged
- Auto-detection at startup
- Redactor: exact match + regex patterns, single chokepoint

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
- File sending support
- Inline keyboard support (for approvals)
- Message routing to agent sessions

**Task 6: Agent Loop + Tools**
Files: agent/*, tools/*
- Session router (per-session Tokio tasks, try_send)
- Agent loop: context assemble → LLM call → tool routing → execute → observe
- Context assembler with trimming + retry
- Policy gate + egress rules
- Non-blocking approval manager (short-ID callbacks)
- Budget tracker (atomic, per-session + daily)
- 8 core tool implementations
- Dynamic tool registry (watch /scripts/*.json, hot-reload)
- create_tool implementation (write files + git commit)
- Dynamic tool execution (JSON stdin → sandbox → JSON stdout)
- Dynamic tool selection (top N by relevance/recency)
- Browser bridge: Playwright subprocess, auto-detection, domain policy
  (skip if headless — tool not registered)

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
- Health self-checks + structured logging
- All Telegram commands

### v1.1 (post-launch)

- **Flatline** — supervisor process (implemented). See `doc/FLATLINE.md`.
  Separate Cargo workspace member (`flatline/`), reads logs + health.json
  + git history, detects 8 failure patterns, diagnoses novel problems via
  LLM, quarantines bad tools, restarts crashed process, proposes fixes
  via Telegram. Own LLM budget (cheap model), own SQLite state.db.
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