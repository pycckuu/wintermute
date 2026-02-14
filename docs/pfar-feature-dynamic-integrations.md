# PFAR Feature Spec: Dynamic Integrations via MCP

> **Feature**: Add service integrations conversationally using MCP servers in sandboxed isolation  
> **Status**: Implementation-ready  
> **Priority**: Phase 3  
> **Depends on**: Pipeline (Phase 2), Vault (Phase 1)

---

## 1. Problem

Every integration requires a Rust module compiled into the binary. The owner can't add services conversationally. Meanwhile, thousands of MCP servers exist for every major service. We should use them — but MCP servers are unsandboxed child processes with full user privileges. No MCP host (Claude Desktop, Cursor, VS Code) sandboxes them today. PFAR should.

---

## 2. Design

One concept: **MCP servers in Bubblewrap sandboxes with proxy-based network filtering.**

The kernel spawns MCP servers as long-lived child processes inside bubblewrap sandboxes. Each server gets network access only to its declared domains (via an HTTP proxy). Credentials come from the vault via environment variables. The kernel discovers tools via MCP's `tools/list`, registers them in the tool registry, and routes calls through the normal pipeline.

No TOML manifest per-action. No custom HTTP template engine. No script runner tier. The MCP server handles all complexity (pagination, OAuth, response transformation, webhooks). PFAR just spawns, sandboxes, discovers, and calls.

---

## 3. Server Configuration

One TOML file per MCP server in `~/.pfar/mcp/`:

```toml
# ~/.pfar/mcp/notion.toml

name = "notion"
description = "Notion workspace — pages, databases, blocks"
label = "internal"                        # security label for all tool outputs
allowed_domains = ["api.notion.com"]      # proxy allowlist

[server]
command = "node"
args = ["/home/user/.pfar/mcp-servers/notion/index.js"]
# OR for npx (slower cold start):
# command = "npx"
# args = ["-y", "@modelcontextprotocol/server-notion"]

[auth]
# env vars injected from vault at spawn time
NOTION_TOKEN = "vault:notion_token"

[sandbox]
# defaults — override only if needed
memory_limit = "256m"
read_only_fs = true
allow_tmp = true                          # writable /tmp inside sandbox
```

That's it. No action definitions — MCP's `tools/list` handles discovery.

---

## 4. Sandbox: Bubblewrap + Proxy

Bubblewrap (bwrap) adds ~3-5ms overhead. MCP servers are long-lived, so this is a one-time cost at spawn. Node.js startup (100-500ms) dominates. The sandbox is invisible in practice.

### 4.1 What the sandbox provides

```
┌──────────────────────────────────────────┐
│  Host                                     │
│                                           │
│  ┌─────────────────────────────────────┐  │
│  │  Bubblewrap Sandbox                  │  │
│  │                                      │  │
│  │  • New network namespace (no net)    │  │
│  │  • New PID namespace                 │  │
│  │  • Read-only filesystem bind mounts  │  │
│  │  • Writable /tmp only                │  │
│  │  • No access to ~/.pfar/vault/       │  │
│  │  • HTTP_PROXY → socat → host proxy   │  │
│  │                                      │  │
│  │  ┌──────────────────────────────┐    │  │
│  │  │  MCP Server (Node.js)        │    │  │
│  │  │  stdin/stdout ← JSON-RPC →   │────│──│── Kernel
│  │  │  HTTP_PROXY=http://localhost  │    │  │
│  │  │      → socat bridge           │────│──│── Domain Proxy (host)
│  │  └──────────────────────────────┘    │  │       ↓
│  └─────────────────────────────────────┘  │   allowed_domains only
│                                           │
└──────────────────────────────────────────┘
```

### 4.2 Spawn sequence

```rust
pub fn spawn_mcp_server(&self, config: &McpServerConfig) -> Result<McpServer> {
    // 1. Resolve credentials from vault → env vars
    let mut env = HashMap::new();
    for (env_name, vault_ref) in &config.auth {
        let secret = self.vault.get_secret(vault_ref.strip_prefix("vault:")?)?;
        env.insert(env_name.clone(), secret);
    }

    // 2. Start domain proxy on a Unix socket
    let proxy_socket = format!("/tmp/pfar-proxy-{}.sock", config.name);
    let proxy = DomainProxy::start(&proxy_socket, &config.allowed_domains)?;

    // 3. Build bwrap command
    let bwrap_args = vec![
        "--unshare-net",                    // new network namespace — no connectivity
        "--unshare-pid",                    // new PID namespace
        "--die-with-parent",               // kill if kernel dies
        "--ro-bind", "/usr", "/usr",        // read-only system
        "--ro-bind", "/lib", "/lib",
        "--ro-bind", "/lib64", "/lib64",
        "--ro-bind", "/bin", "/bin",
        "--ro-bind", "/etc/resolv.conf", "/etc/resolv.conf",
        "--tmpfs", "/tmp",                  // writable tmp
        "--dev", "/dev",
        // Mount the MCP server code read-only
        "--ro-bind", &server_path, &server_path,
        // Mount Node.js/npm if needed
        "--ro-bind", &node_path, &node_path,
        // Proxy env vars
        "--setenv", "HTTP_PROXY", "http://127.0.0.1:9876",
        "--setenv", "HTTPS_PROXY", "http://127.0.0.1:9876",
        // NO bind mount for ~/.pfar/vault/
    ];

    // 4. Inject credentials as env vars
    for (k, v) in &env {
        bwrap_args.extend(["--setenv", k, v]);
    }

    // 5. Spawn: bwrap [...] -- node index.js
    let child = Command::new("bwrap")
        .args(&bwrap_args)
        .arg("--")
        .arg(&config.server.command)
        .args(&config.server.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // 6. MCP handshake: initialize → tools/list
    let transport = StdioTransport::new(child.stdin, child.stdout);
    let tools = mcp_initialize(&transport)?;

    // 7. Register discovered tools
    for tool in tools {
        self.tool_registry.register(McpTool {
            name: format!("{}.{}", config.name, tool.name),
            description: tool.description,
            input_schema: tool.input_schema,
            label: config.label.clone(),
            semantics: infer_semantics(&tool),
            server_name: config.name.clone(),
        });
    }

    Ok(McpServer { transport, child, proxy })
}
```

### 4.3 Domain proxy

A simple HTTP CONNECT proxy on the host side of the socat bridge. ~100 lines of Rust.

```rust
pub struct DomainProxy {
    allowed_domains: HashSet<String>,
}

impl DomainProxy {
    pub fn handle_connect(&self, target_host: &str) -> Result<()> {
        if !self.allowed_domains.contains(target_host) {
            log::warn!("Blocked: MCP server tried to reach {}", target_host);
            return Err(ProxyError::DomainBlocked(target_host.into()));
        }
        Ok(()) // forward the connection
    }
}
```

This recovers ScopedHttpClient-equivalent domain restriction. The MCP server thinks it has normal network access, but every outbound connection is filtered.

### 4.4 Semantics inference

MCP tools have optional annotations. The kernel maps them to PFAR semantics:

```rust
fn infer_semantics(tool: &McpToolDef) -> ToolSemantics {
    match (tool.annotations.read_only_hint, tool.annotations.destructive_hint) {
        (Some(true), _) => ToolSemantics::Read,
        (_, Some(true)) => ToolSemantics::Write,  // triggers taint/approval
        _ => ToolSemantics::Write,  // default to write (safe default)
    }
}
```

Conservative: if unannotated, assume write. Owner can override per-tool in TOML if needed.

---

## 5. Tool Execution in the Pipeline

MCP tools go through the same 4-phase pipeline as built-in tools:

```
Owner: "Search Notion for the PFAR design doc"
→ Phase 0: Extract {intent: "search", entities: ["Notion", "PFAR design doc"]}
→ Phase 1: Plan [{tool: "notion.search", args: {query: "PFAR design doc"}}]
→ Phase 2: Kernel sends JSON-RPC to Notion MCP server via stdio:
    {"jsonrpc": "2.0", "method": "tools/call",
     "params": {"name": "search", "arguments": {"query": "PFAR design doc"}}}
   MCP server → api.notion.com (via proxy — allowed) → returns results
→ Phase 3: Synthesizer formats response for owner
```

Security properties preserved:
- **Plan-Then-Execute**: Planner selects, kernel executes
- **Credentials invisible to LLM**: env vars in sandbox, never in prompts
- **Domain isolation**: proxy blocks non-allowed domains
- **Taint on writes**: destructive tools trigger approval flow
- **Label enforcement**: all outputs labeled per config
- **Audit**: every `tools/call` + response logged

---

## 6. Conversational Setup

### 6.1 Built-in registry

Ship with templates for ~20 common MCP servers:

```rust
const KNOWN_MCP_SERVERS: &[KnownServer] = &[
    KnownServer {
        name: "notion",
        package: "@modelcontextprotocol/server-notion",
        domains: &["api.notion.com"],
        credentials: &[("NOTION_TOKEN",
            "Go to notion.so/profile/integrations → Create → Copy secret")],
        default_label: "internal",
    },
    KnownServer {
        name: "github",
        package: "@modelcontextprotocol/server-github",
        domains: &["api.github.com"],
        credentials: &[("GITHUB_PERSONAL_ACCESS_TOKEN",
            "Go to github.com/settings/tokens → Fine-grained → Copy")],
        default_label: "internal",
    },
    // slack, linear, jira, google-drive, postgres, ...
];
```

### 6.2 Known service flow

```
Owner: "Connect Notion"
→ Agent finds "notion" in registry
→ "To connect Notion, I need an integration token.
    Go to notion.so/profile/integrations → Create integration → copy the secret."

Owner: "ntn_v2_abc123xyz..."
→ Stores "notion_token" in vault
→ npm install -g @modelcontextprotocol/server-notion
→ Writes ~/.pfar/mcp/notion.toml
→ Spawns in bwrap sandbox
→ tools/list → discovers 12 tools
→ "Notion connected. I can search pages, read databases, create pages,
    and manage blocks. Want me to test with a search?"
```

### 6.3 Unknown service flow

```
Owner: "Connect to our internal tracker at tracker.mycompany.com"
→ Agent: "Is there an MCP server package for it on npm?
    If not, I can use a generic HTTP server."

Owner: "No, just use generic"
→ Sets up @modelcontextprotocol/server-fetch
→ allowed_domains = ["tracker.mycompany.com"]
→ Asks for auth token → stores in vault
→ Done
```

### 6.4 Pre-installation

To avoid npx cold-start delays (5-30s), install packages globally during setup:

```bash
npm install -g @modelcontextprotocol/server-notion
```

TOML points to installed binary. Spawns drop from 5-30s to ~300ms.

---

## 7. Server Lifecycle

MCP servers are **long-lived** — spawned once, kept running.

```
Startup:
  → Read ~/.pfar/mcp/*.toml
  → For each with credentials in vault: spawn in bwrap → handshake → register tools

Operation:
  → Kernel sends tools/call via stdin, reads stdout
  → Handle notifications/tools/list_changed → re-discover
  → Monitor child: crash → deregister tools → restart with backoff (1s, 5s, 30s)
  → 3 crashes → disable, notify owner

Shutdown:
  → Close stdin → wait 5s → SIGTERM → wait 3s → SIGKILL
  → Kill proxy processes
```

---

## 8. Security Summary

| Property | How |
|---|---|
| Domain isolation | Bwrap `--unshare-net` + socat + domain proxy |
| Credential protection | Vault → env vars. Server can't exfiltrate (proxy blocks). |
| Filesystem isolation | Read-only mounts. No vault access. /tmp only. |
| Process isolation | PID namespace. `--die-with-parent`. |
| Label enforcement | Config label on all outputs. Kernel enforces. |
| Write approval | MCP hints → taint/approval flow. Default: write. |
| Audit | Every tools/call logged. |

### Accepted residual risks
- Compromised MCP server could exfiltrate to its allowed domains (e.g., attacker's Notion workspace). Mitigate: use official packages, pin versions.
- Credential in server memory could be dumped with code execution inside sandbox. Mitigate: PID namespace, but not bulletproof.
- MCP server could cache data in /tmp. Mitigate: tmpfs, cleared on restart.

---

## 9. Performance Budget

### Per-request (steady state)

| Step | Time |
|---|---|
| Phase 0 (Extract) | ~200ms |
| Phase 1 (Plan) | ~500ms |
| Kernel → MCP (stdio pipe) | ~1ms |
| MCP → API (via proxy) | ~200-500ms |
| Proxy overhead | ~1-2ms |
| Phase 3 (Synthesize) | ~500ms |
| **Total** | **~1.5-2s** |

Proxy adds ~1-2ms. Bwrap adds 0ms (already spawned). Both invisible next to LLM inference.

### Server spawn (one-time at startup)

| Step | Time |
|---|---|
| Bwrap + socat | ~5ms |
| Node.js startup | ~100-300ms |
| MCP handshake + tools/list | ~50-100ms |
| **Total per server** | **~200-500ms** |

5 servers → ~1-2.5s added to PFAR startup. Acceptable.

---

## 10. Implementation Checklist

### Bubblewrap sandbox
- [ ] `BwrapSandbox` — builds bwrap command from config
- [ ] Socat bridge (Unix socket ↔ localhost inside namespace)
- [ ] `--die-with-parent`, `--unshare-net`, `--unshare-pid`
- [ ] Read-only bind mounts, exclude vault directory
- [ ] Writable /tmp as tmpfs

### Domain proxy
- [ ] HTTP CONNECT proxy with domain allowlist (~100 LOC)
- [ ] Listens on Unix socket, bridged via socat
- [ ] Logs blocked domains to audit log
- [ ] HTTPS CONNECT tunneling support

### MCP client
- [ ] `StdioTransport` — JSON-RPC 2.0 over stdin/stdout
- [ ] `mcp_initialize()` — capability negotiation
- [ ] `mcp_list_tools()` — discover and register tools
- [ ] `mcp_call_tool()` — invoke tool, return result
- [ ] Handle `notifications/tools/list_changed`
- [ ] Graceful shutdown sequence

### Server lifecycle
- [ ] Load `~/.pfar/mcp/*.toml` at startup
- [ ] Spawn with credentials from vault as env vars
- [ ] Monitor, auto-restart with backoff (1s → 5s → 30s → disable)
- [ ] Deregister/re-register tools on crash/restart

### Pipeline integration
- [ ] `McpTool` implementing Tool trait
- [ ] Semantics from MCP annotations (default: write)
- [ ] Labels from config
- [ ] Taint/approval for write tools
- [ ] Audit logging

### Built-in registry + conversational setup
- [ ] ~20 known server templates
- [ ] "Connect [service]" intent → registry lookup → credential prompt → install → spawn
- [ ] Unknown service → generic HTTP MCP server fallback
- [ ] Pre-install packages globally (avoid npx cold start)

### Tests
- [ ] Server spawns in sandbox, tools discovered via MCP
- [ ] Proxy blocks non-allowed domains
- [ ] Proxy allows declared domains
- [ ] Credentials in env vars, not in tool registry or prompts
- [ ] Write tool triggers approval flow
- [ ] Crash → restart → tools re-registered
- [ ] 3 crashes → disabled, owner notified
- [ ] 5 servers spawn in <3s total
- [ ] Per-request proxy overhead <5ms
- [ ] "Connect Notion" conversational flow end-to-end
