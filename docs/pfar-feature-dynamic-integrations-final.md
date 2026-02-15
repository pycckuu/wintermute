# PFAR Feature Spec: Dynamic Integrations

> **Feature**: Add service integrations conversationally — sandboxed MCP servers, secure credential acquisition, kernel-managed setup flow  
> **Status**: Implementation-ready  
> **Priority**: Phase 3  
> **Depends on**: Pipeline (Phase 2), Vault (Phase 1)

---

## 1. Problem

Three problems, one spec.

**No runtime extensibility.** Every integration requires a Rust module compiled into the binary. The owner can't add services conversationally.

**Credential acquisition is broken.** When the owner pastes a token in chat, the pipeline can't handle it: intent extractor returns `None`, fast path activates, synthesizer hallucinates. Sending credentials through the LLM pipeline is also a security violation — cloud providers log context windows, prompt injection can extract secrets.

**Setup flow has no continuity.** Even if the credential is captured, nothing triggers the next step. The owner must manually re-trigger "connect notion" because the pipeline is stateless single-turn. There's no concept of "credential stored, now auto-connect."

---

## 2. Design Overview

Three components, working together:

**MCP servers in Bubblewrap sandboxes.** The kernel spawns MCP servers as long-lived child processes inside network-isolated sandboxes. Each server can only reach its declared domains via an HTTP proxy. The MCP server handles all integration complexity (pagination, OAuth refresh, response transformation). PFAR just spawns, sandboxes, discovers tools, and calls them through the normal pipeline.

**KernelFlow state machine.** Integration setup is a multi-step flow that runs alongside the pipeline, not through it. A kernel-managed state machine handles: check registry → acquire credential → spawn server → verify → report. No LLM is involved in any step.

**Credential acquisition with three methods.** OAuth device flow (token never in chat), local web form (token stays on localhost), in-chat paste with immediate deletion (fallback). All three converge on the same flow transition: credential received → spawn server.

```
Owner: "connect notion"
  │
  ▼
┌──────────────────────────────────────────────────────────────┐
│ KernelFlow (not the pipeline)                                 │
│                                                               │
│  Start ──► AwaitingCredential ──► Spawning ──► Verifying ──► Complete
│              │                      │            │                    │
│              │ (credential arrives   │ (bwrap +   │ (test API call)   │
│              │  via gate/web/oauth)  │  MCP init) │                   │
│              │                      │            │                    │
│              │ delete msg from chat  │ resolve    │ on failure:       │
│              │ store in vault        │ vault refs │ re-prompt token   │
│              │                      │ inject env │                   │
└──────────────────────────────────────────────────────────────┘
                                                        │
                                                        ▼
                                                "✓ Notion connected.
                                                 12 tools available."
                                                        │
                                                        ▼
                                          Tools registered in pipeline.
                                          Normal messages use them via
                                          Phase 0 → 1 → 2 → 3.
```

---

## 3. MCP Server Configuration

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

[auth]
NOTION_TOKEN = "vault:notion_token"       # resolved at spawn time

[sandbox]
memory_limit = "256m"
read_only_fs = true
allow_tmp = true
```

No action definitions — MCP's `tools/list` handles discovery.

---

## 4. Bubblewrap Sandbox

Each MCP server runs in a Bubblewrap sandbox with network isolation. Bwrap adds ~3-5ms at spawn (one-time). Node.js startup (100-300ms) dominates. The sandbox is invisible in practice.

### What the sandbox provides

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

### Domain proxy

~100 lines of Rust. HTTP CONNECT proxy on the host side of the socat bridge. The MCP server thinks it has normal network access, but every outbound connection is filtered by `allowed_domains`.

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
        Ok(())
    }
}
```

This recovers ScopedHttpClient-equivalent domain restriction for MCP servers. No other MCP host does this.

### Semantics inference

MCP tools have optional annotations. The kernel maps them to PFAR semantics:

```rust
fn infer_semantics(tool: &McpToolDef) -> ToolSemantics {
    match (tool.annotations.read_only_hint, tool.annotations.destructive_hint) {
        (Some(true), _) => ToolSemantics::Read,
        (_, Some(true)) => ToolSemantics::Write,  // triggers taint/approval
        _ => ToolSemantics::Write,                 // safe default
    }
}
```

---

## 5. KernelFlow: Integration Setup State Machine

Integration setup runs **alongside the pipeline, not through it**. The pipeline is single-turn request/response. Setup is multi-turn with waits. Mixing them produces dead ends.

### States

```rust
pub enum FlowState {
    /// Check registry, check vault.
    Start,

    /// Waiting for credential. Flow is paused.
    AwaitingCredential {
        method: AcquisitionMethod,
        prompted_at: Instant,
        ttl: Duration,               // 10 minutes
    },

    /// Credential received. Spawning MCP server.
    Spawning,

    /// Server spawned. Test API call in progress.
    Verifying,

    /// Test call failed. Credential probably wrong.
    CredentialInvalid {
        error: String,
        retry_count: u8,             // max 2 retries
    },

    /// Integration is live.
    Complete,

    /// Unrecoverable error.
    Failed { error: String },
}
```

### The flow manager

```rust
pub struct KernelFlowManager {
    active_flows: HashMap<PrincipalId, KernelFlow>,
    vault: Arc<Vault>,
    gateway: Arc<dyn Gateway>,
    mcp_manager: Arc<McpServerManager>,
    registry: Arc<ServiceRegistry>,
    web_server: Option<CredentialWebServer>,
}

pub struct KernelFlow {
    pub service: String,
    pub state: FlowState,
    pub config: ServiceConfig,
    pub principal: PrincipalId,
    pub created_at: Instant,
}
```

### State machine driver

```rust
impl KernelFlowManager {
    /// Advance the flow to its next state and execute the entry action.
    async fn advance(&mut self, principal: &PrincipalId) -> Result<()> {
        let flow = self.active_flows.get_mut(principal)
            .ok_or(FlowError::NoActiveFlow)?;

        match &flow.state {
            FlowState::Start => {
                // Check if credential already in vault
                if self.vault.has_secret(&flow.config.vault_key)? {
                    flow.state = FlowState::Spawning;
                    return self.advance(principal).await;
                }

                // Need credential
                let method = self.pick_acquisition_method(&flow.config);
                self.request_credential(flow, &method).await?;
                flow.state = FlowState::AwaitingCredential {
                    method,
                    prompted_at: Instant::now(),
                    ttl: Duration::from_secs(600),
                };
                // PAUSED — waiting for input
                Ok(())
            }

            FlowState::Spawning => {
                self.gateway.send(&flow.principal,
                    &format!("Credential stored. Connecting to {}...", flow.service)
                ).await;

                match self.spawn_mcp_server(flow).await {
                    Ok(_tools) => {
                        flow.state = FlowState::Verifying;
                        self.advance(principal).await
                    }
                    Err(e) => {
                        let msg = format!("Failed to start {} server: {}", flow.service, e);
                        flow.state = FlowState::Failed { error: msg.clone() };
                        self.gateway.send(&flow.principal, &msg).await;
                        self.active_flows.remove(principal);
                        Ok(())
                    }
                }
            }

            FlowState::Verifying => {
                match self.verify_connection(flow).await {
                    Ok(()) => {
                        let tool_count = self.mcp_manager
                            .get_tools(&flow.service)
                            .map(|t| t.len()).unwrap_or(0);

                        self.gateway.send(&flow.principal, &format!(
                            "✓ {} connected. {} tools available.", flow.service, tool_count
                        )).await;

                        flow.state = FlowState::Complete;
                        self.active_flows.remove(principal);
                        Ok(())
                    }
                    Err(e) => {
                        let retry_count = match &flow.state {
                            FlowState::CredentialInvalid { retry_count, .. } => *retry_count,
                            _ => 0,
                        };

                        if retry_count >= 2 {
                            flow.state = FlowState::Failed {
                                error: format!("3 attempts failed: {}", e),
                            };
                            self.gateway.send(&flow.principal, &format!(
                                "{} setup failed after 3 attempts: {}. Cancelled.",
                                flow.service, e
                            )).await;
                            self.cleanup_failed_setup(flow).await;
                            self.active_flows.remove(principal);
                            return Ok(());
                        }

                        flow.state = FlowState::CredentialInvalid {
                            error: e.to_string(),
                            retry_count: retry_count + 1,
                        };
                        self.gateway.send(&flow.principal, &format!(
                            "The {} token doesn't work ({}). Send a new token, or 'cancel'.",
                            flow.service, e
                        )).await;
                        // PAUSED — waiting for new credential
                        Ok(())
                    }
                }
            }

            // Terminal or paused states — nothing to advance
            _ => Ok(()),
        }
    }
}
```

---

## 6. Credential Acquisition

Three methods, tried in preference order. All converge on the same transition: credential received → `flow.state = Spawning` → `advance()`.

### Method 1: OAuth Device Flow (best)

Token never appears in chat. Works for GitHub, Google, Microsoft.

```
Owner: "connect github"
→ "Open this link and enter the code:
    🔗 https://github.com/login/device
    📋 Code: WDJB-MJHT"
→ Kernel polls token endpoint every 5s
→ Owner authorizes in browser
→ Token received → vault → spawn → verify → done
```

```rust
fn spawn_device_flow_poller(&self, flow_principal: PrincipalId, config: &OAuthConfig) {
    let mgr = self.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(config.interval)).await;
            match poll_token_endpoint(config).await {
                Ok(tokens) => {
                    mgr.vault.store_secret(&config.vault_key, &tokens.access_token).unwrap();
                    if let Some(flow) = mgr.active_flows.get_mut(&flow_principal) {
                        flow.state = FlowState::Spawning;
                        mgr.advance(&flow_principal).await.ok();
                    }
                    return;
                }
                Err(PollError::Pending) => continue,
                Err(PollError::SlowDown) => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
                Err(_) => {
                    mgr.gateway.send(&flow_principal,
                        "OAuth failed. Send the token directly instead."
                    ).await;
                    return; // flow stays in AwaitingCredential — paste fallback
                }
            }
        }
    });
}
```

### Method 2: Local Web Form (good)

Token stays on localhost. Never enters Telegram. `<input type="password">` masks input.

```
Owner: "connect notion"
→ "Enter your token securely: http://localhost:19275/credential/notion
    Or just paste it here."
→ Owner opens link, pastes token in password field
→ Form POSTs to localhost → vault → spawn → verify → done
```

Localhost-only binding, CSRF nonce, 10-minute expiry, self-contained HTML with no external resources.

```rust
async fn handle_web_form_post(&self, service: &str, nonce: &str, token: &str) -> Result<()> {
    // Validate nonce + expiry
    let pending = self.pending_forms.remove(&format!("{}:{}", service, nonce))
        .ok_or(Error::InvalidNonce)?;

    // Store in vault
    self.vault.store_secret(&pending.vault_key, token)?;

    // Advance the flow
    if let Some(flow) = self.flow_manager.active_flows.get_mut(&pending.principal) {
        flow.state = FlowState::Spawning;
        self.flow_manager.advance(&pending.principal).await?;
    }
    Ok(())
}
```

### Method 3: In-Chat Paste with Kernel Intercept (fallback)

For mobile users or when the web form isn't reachable. Token briefly in chat, deleted immediately.

```
Owner: "connect notion"
→ "Send me the token. I'll delete the message immediately."
Owner: "ntn_v2_abc123..."
→ Intercepted before Phase 0
→ Stored in vault
→ Message deleted from chat
→ Spawn → verify → done
```

The intercept happens in the flow manager, not a separate subsystem:

```rust
impl KernelFlowManager {
    /// Called for every incoming message BEFORE the pipeline.
    /// Returns true if the message was consumed by a flow.
    pub async fn intercept(&mut self, msg: &IncomingMessage) -> bool {
        let Some(flow) = self.active_flows.get_mut(&msg.principal_id) else {
            return false;
        };

        // Only intercept when waiting for credential
        match &flow.state {
            FlowState::AwaitingCredential { .. }
            | FlowState::CredentialInvalid { .. } => {}
            _ => return false,
        }

        let text = msg.text.trim();

        // Cancel?
        let lower = text.to_lowercase();
        if lower == "cancel" || lower == "nevermind" || lower == "skip" {
            self.gateway.send(&msg.principal_id,
                &format!("{} setup cancelled.", flow.service)
            ).await;
            self.active_flows.remove(&msg.principal_id);
            return true;
        }

        // Credential or normal message?
        if !self.looks_like_credential(text, &flow.config) {
            return false; // let pipeline handle it
        }

        // ── It's a credential ──

        // 1. Store in vault
        if let Err(e) = self.vault.store_secret(&flow.config.vault_key, text) {
            self.gateway.send(&msg.principal_id, "Failed to store credential.").await;
            return true;
        }

        // 2. Delete from chat (best effort)
        if let Err(e) = self.gateway.delete_message(&msg.principal_id, msg.message_id).await {
            self.gateway.send(&msg.principal_id,
                "⚠️ Couldn't delete your token message. Delete it manually."
            ).await;
        }

        // 3. Advance: AwaitingCredential → Spawning → Verifying → Complete
        flow.state = FlowState::Spawning;
        self.advance(&msg.principal_id).await.ok();

        true
    }

    fn looks_like_credential(&self, text: &str, config: &ServiceConfig) -> bool {
        // Known prefix
        if let Some(prefix) = &config.expected_prefix {
            if text.starts_with(prefix) { return true; }
        }
        // Known regex
        if let Some(pattern) = &config.token_pattern {
            if pattern.is_match(text) { return true; }
        }
        // Heuristic: no spaces, >15 chars, mostly alphanumeric
        if text.len() < 15 || text.len() > 500 { return false; }
        if text.contains(' ') || text.contains('\n') { return false; }
        let ratio = text.chars()
            .filter(|c| c.is_alphanumeric() || "-_.:=+/".contains(*c))
            .count() as f64 / text.len() as f64;
        ratio > 0.9
    }
}
```

### Known token patterns

```rust
const TOKEN_PATTERNS: &[(&str, &str)] = &[
    ("notion",    "ntn_"),
    ("github",    "ghp_"),
    ("github",    "github_pat_"),
    ("openai",    "sk-"),
    ("slack",     "xoxb-"),
    ("anthropic", "sk-ant-"),
    ("linear",    "lin_api_"),
    ("stripe",    "sk_live_"),
    ("stripe",    "sk_test_"),
    ("sendgrid",  "SG."),
];
```

---

## 7. MCP Server Spawn with Vault Resolution

The third root cause from the bug trace: `vault:` refs must be resolved to actual values.

```rust
impl KernelFlowManager {
    async fn spawn_mcp_server(&self, flow: &KernelFlow) -> Result<Vec<ToolDef>> {
        // 1. Resolve vault references → actual secret values
        let mut env: HashMap<String, String> = HashMap::new();
        for (env_name, vault_ref) in &flow.config.auth {
            let key = vault_ref.strip_prefix("vault:")
                .ok_or(FlowError::InvalidVaultRef(vault_ref.clone()))?;
            let secret = self.vault.get_secret(key)?;
            env.insert(env_name.clone(), secret);
        }

        // 2. Build bwrap command
        let bwrap_args = vec![
            "--unshare-net",
            "--unshare-pid",
            "--die-with-parent",
            "--ro-bind", "/usr", "/usr",
            "--ro-bind", "/lib", "/lib",
            "--ro-bind", "/lib64", "/lib64",
            "--ro-bind", "/bin", "/bin",
            "--ro-bind", "/etc/resolv.conf", "/etc/resolv.conf",
            "--tmpfs", "/tmp",
            "--dev", "/dev",
            "--ro-bind", &server_path, &server_path,
            "--ro-bind", &node_path, &node_path,
            "--setenv", "HTTP_PROXY", "http://127.0.0.1:9876",
            "--setenv", "HTTPS_PROXY", "http://127.0.0.1:9876",
            // NO bind mount for ~/.pfar/vault/
        ];

        // 3. Inject resolved credentials as env vars
        for (k, v) in &env {
            bwrap_args.extend(["--setenv", k, v]);
        }

        // 4. Start domain proxy
        let proxy = DomainProxy::start(&flow.config.allowed_domains)?;

        // 5. Spawn
        let child = Command::new("bwrap")
            .args(&bwrap_args)
            .arg("--")
            .arg(&flow.config.command)
            .args(&flow.config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // 6. MCP handshake
        let transport = StdioTransport::new(child.stdin, child.stdout);
        let tools = mcp_initialize(&transport)?;

        // 7. Register tools in pipeline
        for tool in &tools {
            self.tool_registry.register(McpTool {
                name: format!("{}.{}", flow.config.name, tool.name),
                description: tool.description.clone(),
                input_schema: tool.input_schema.clone(),
                label: flow.config.label.clone(),
                semantics: infer_semantics(tool),
                server_name: flow.config.name.clone(),
            });
        }

        Ok(tools)
    }

    async fn verify_connection(&self, flow: &KernelFlow) -> Result<()> {
        let tools = self.mcp_manager.get_tools(&flow.service)
            .ok_or(FlowError::NoTools)?;

        // Pick a read-only tool for the test call
        let test_tool = tools.iter()
            .find(|t| t.semantics == ToolSemantics::Read)
            .or_else(|| tools.first())
            .ok_or(FlowError::NoTools)?;

        let result = self.mcp_manager
            .call_tool(&flow.service, &test_tool.name, &serde_json::json!({}))
            .await?;

        if result.is_error {
            return Err(FlowError::VerificationFailed(
                result.content.first()
                    .and_then(|c| c.as_text())
                    .unwrap_or("unknown error")
                    .to_string()
            ));
        }

        Ok(())
    }

    async fn cleanup_failed_setup(&self, flow: &KernelFlow) {
        self.mcp_manager.stop_server(&flow.service).await.ok();
        // Keep credential in vault — owner might retry
    }
}
```

---

## 8. Integration with main.rs

```rust
loop {
    let msg = gateway.recv().await?;

    // 1. Active flow intercept (credential paste, cancel)
    if flow_manager.intercept(&msg).await {
        continue;
    }

    // 2. Flow-starting command (bypasses pipeline entirely)
    if let Some(service) = parse_connect_command(&msg.text) {
        flow_manager.start_setup(&service, &msg.principal_id).await?;
        continue;
    }

    // 3. Normal pipeline
    let extraction = phase0_extract(&msg).await?;
    if should_use_full_pipeline(&extraction) {
        let plan = phase1_plan(&extraction).await?;
        let results = phase2_execute(&plan).await?;
        let response = phase3_synthesize(&results).await?;
        gateway.send(&msg.principal_id, &response).await;
    } else {
        let response = fast_path_synthesize(&msg, &extraction).await?;
        gateway.send(&msg.principal_id, &response).await;
    }
}

fn parse_connect_command(text: &str) -> Option<String> {
    let lower = text.to_lowercase().trim().to_string();
    let prefixes = ["connect ", "add ", "setup ", "set up ", "integrate ", "enable "];
    for prefix in &prefixes {
        if lower.starts_with(prefix) {
            let service = lower[prefix.len()..].trim().to_string();
            if !service.is_empty() { return Some(service); }
        }
    }
    None
}
```

Flow-starting commands bypass the pipeline. Other admin commands (list, remove, status) still go through it. Normal messages always go through it. Clean separation.

---

## 9. Built-in Registry

```rust
const KNOWN_SERVERS: &[ServiceConfig] = &[
    ServiceConfig {
        name: "notion",
        package: "@modelcontextprotocol/server-notion",
        command: "node",
        domains: &["api.notion.com"],
        vault_key: "notion_token",
        expected_prefix: Some("ntn_"),
        default_label: "internal",
        auth: &[("NOTION_TOKEN", "vault:notion_token")],
        auth_methods: &[
            AuthMethod::PasteToken {
                instructions: "notion.so/profile/integrations → Create → Copy secret",
            },
        ],
    },
    ServiceConfig {
        name: "github",
        package: "@modelcontextprotocol/server-github",
        command: "node",
        domains: &["api.github.com"],
        vault_key: "github_token",
        expected_prefix: Some("ghp_"),
        default_label: "internal",
        auth: &[("GITHUB_PERSONAL_ACCESS_TOKEN", "vault:github_token")],
        auth_methods: &[
            AuthMethod::OAuthDeviceFlow {
                device_auth_url: "https://github.com/login/device/code",
                token_url: "https://github.com/login/oauth/access_token",
                client_id: "PFAR_GITHUB_CLIENT_ID",
                scopes: &["repo", "read:org"],
            },
            AuthMethod::PasteToken {
                instructions: "github.com/settings/tokens → Fine-grained → Copy",
            },
        ],
    },
    // slack, linear, jira, google-drive, postgres, ...
];
```

---

## 10. Server Lifecycle

MCP servers are long-lived — spawned once, kept running while PFAR runs.

```
Startup:
  → Read ~/.pfar/mcp/*.toml
  → For each with credentials in vault: spawn in bwrap → handshake → register tools

During operation:
  → Kernel sends tools/call via stdin, reads stdout
  → Handle notifications/tools/list_changed → re-discover
  → Monitor: crash → deregister tools → restart with backoff (1s, 5s, 30s)
  → 3 crashes → disable, notify owner

Shutdown:
  → Close stdin → wait 5s → SIGTERM → wait 3s → SIGKILL
  → Kill proxy processes
```

---

## 11. Pipeline Integration for Tool Calls

Once setup completes, MCP tools go through the normal 4-phase pipeline:

```
Owner: "Search Notion for the PFAR design doc"
→ Phase 0: Extract {intent: "search", entities: ["Notion", "PFAR design doc"]}
→ Phase 1: Plan [{tool: "notion.search", args: {query: "PFAR design doc"}}]
→ Phase 2: Kernel sends JSON-RPC to Notion MCP server via stdio
   MCP server → api.notion.com (via proxy — allowed) → returns results
→ Phase 3: Synthesizer formats response
```

Security properties preserved during tool calls:

- **Plan-Then-Execute**: Planner selects, kernel executes
- **Credentials invisible to LLM**: env vars in sandbox, never in prompts
- **Domain isolation**: proxy blocks non-allowed domains
- **Taint on writes**: destructive tools trigger approval flow
- **Label enforcement**: all outputs labeled per config
- **Audit**: every tools/call + response logged

---

## 12. The Full Trace (Fixed)

**"connect notion"**
1. `parse_connect_command()` → `Some("notion")`
2. `flow_manager.start_setup("notion")` — pipeline never runs
3. Flow: Start → vault miss → AwaitingCredential
4. Sends: "I need your Notion token. notion.so/profile/integrations → Create → Copy. Paste it here."

**User pastes "ntn_265011..."**
1. `flow_manager.intercept()` → flow in AwaitingCredential
2. `looks_like_credential("ntn_265...")` → true (prefix match)
3. `vault.store_secret("notion_token", "ntn_265011...")` → OK
4. `gateway.delete_message(msg.message_id)` → token gone from chat
5. `flow.state = Spawning` → `advance()` →
6. `spawn_mcp_server()` → resolves `vault:notion_token` → injects as `NOTION_TOKEN` → bwrap sandbox → MCP handshake → 12 tools discovered
7. `flow.state = Verifying` → `advance()` →
8. `verify_connection()` → calls `notion.search({})` → 200 OK
9. Sends: "✓ Notion connected. 12 tools available."
10. Flow complete, removed.

**"search notion for PFAR design doc"**
1. `flow_manager.intercept()` → no active flow → false
2. `parse_connect_command()` → None
3. Normal pipeline → Phase 0-3 → notion.search tool called → results returned

No dead ends. No re-triggers. No LLM sees the credential.

---

## 13. Security Summary

| Property | How |
|---|---|
| Domain isolation | Bwrap `--unshare-net` + socat + domain proxy |
| Credential protection | Vault → env vars at spawn. Proxy blocks exfiltration. |
| Credential never in LLM context | KernelFlow handles setup. Pipeline never involved. |
| Chat message deleted | `deleteMessage` immediately after vault storage |
| Filesystem isolation | Read-only mounts. No vault access. /tmp only. |
| Process isolation | PID namespace. `--die-with-parent`. |
| Label enforcement | Config label on all outputs. Kernel enforces. |
| Write approval | MCP hints → taint/approval flow. Default: write. |
| Audit | Every credential op + tools/call logged. |

### Accepted residual risks

- Compromised MCP server could exfiltrate to its allowed domains. Mitigate: official packages, pinned versions.
- Credential in server memory could be dumped with code execution inside sandbox. Mitigate: PID namespace.
- Token briefly in Telegram notification history before `deleteMessage`. No mitigation possible.
- MCP server could cache data in /tmp. Mitigate: tmpfs, cleared on restart.

---

## 14. Performance

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

### Server spawn (one-time)

| Step | Time |
|---|---|
| Bwrap + socat | ~5ms |
| Node.js startup | ~100-300ms |
| MCP handshake + tools/list | ~50-100ms |
| **Total per server** | **~200-500ms** |

### Full setup flow (one-time)

| Step | Time |
|---|---|
| "connect notion" → prompt sent | ~10ms |
| Owner pastes token | (human time) |
| Vault store + message delete | ~20ms |
| MCP server spawn | ~200-500ms |
| Verify (test API call) | ~200-500ms |
| **Total (after paste)** | **~0.5-1s** |

---

## 15. Implementation Checklist

### KernelFlowManager
- [ ] `KernelFlow` struct with `FlowState` enum
- [ ] `start_setup()` — create flow, advance from Start
- [ ] `advance()` — state machine driver, all transitions
- [ ] `intercept()` — check messages against active flows
- [ ] `parse_connect_command()` — detect flow-starting messages
- [ ] `tick()` — periodic cleanup of timed-out flows (10 min)

### Credential acquisition
- [ ] `looks_like_credential()` — prefix + regex + heuristic
- [ ] Token pattern registry for known services
- [ ] `handle_credential_input()` — store, delete message, advance flow
- [ ] Gateway `delete_message()` per platform (Telegram: deleteMessage API)
- [ ] Pending deletion recovery log (crash safety)
- [ ] Audit logging (never the credential value)

### MCP spawn with vault resolution
- [ ] Resolve `vault:` prefixes → `vault.get_secret()` → env vars
- [ ] Build bwrap command with resolved env
- [ ] Start domain proxy, socat bridge
- [ ] MCP handshake + `tools/list` → register tools
- [ ] Handle spawn failures → flow transitions to Failed

### Bubblewrap sandbox
- [ ] `--unshare-net`, `--unshare-pid`, `--die-with-parent`
- [ ] Read-only bind mounts, exclude vault directory
- [ ] Writable /tmp as tmpfs
- [ ] Socat bridge (Unix socket ↔ localhost inside namespace)

### Domain proxy
- [ ] HTTP CONNECT proxy with domain allowlist
- [ ] HTTPS tunneling support
- [ ] Blocked domain logging to audit

### Verification + retry
- [ ] `verify_connection()` — find read-only tool, test call
- [ ] Auth failure → CredentialInvalid → re-prompt → new token
- [ ] 3-strike limit → Failed, cleanup

### Server lifecycle
- [ ] Load `~/.pfar/mcp/*.toml` at startup, spawn existing servers
- [ ] Monitor child processes, auto-restart with backoff
- [ ] Handle `notifications/tools/list_changed`
- [ ] Graceful shutdown: close stdin → SIGTERM → SIGKILL

### Built-in registry
- [ ] ~20 known server templates with auth methods
- [ ] Pre-install packages globally (avoid npx cold start)

### OAuth device flow (Phase 3+)
- [ ] Background poller task
- [ ] Token received → vault → advance flow
- [ ] Timeout / failure → fallback to paste

### Local web form (Phase 3+)
- [ ] Localhost HTTP server, `type="password"` input
- [ ] CSRF nonce + 10-min expiry
- [ ] POST handler → vault → advance flow

### Tests
- [ ] Full flow: "connect notion" → paste token → spawn → verify → complete
- [ ] Credential intercepted before Phase 0 (no LLM sees it)
- [ ] Token deleted from chat after storage
- [ ] Normal message during AwaitingCredential → passes to pipeline
- [ ] "cancel" → flow removed
- [ ] Invalid credential → re-prompt → new token → success
- [ ] 3 failures → flow cancelled
- [ ] 10-min timeout → flow cleaned up
- [ ] Vault resolution: `vault:notion_token` → actual token in env
- [ ] MCP spawn failure → flow fails with clear error
- [ ] Proxy blocks non-allowed domains
- [ ] Proxy allows declared domains
- [ ] Credential already in vault → skip to Spawning
- [ ] "connect github" while "connect notion" pending → old cancelled
- [ ] 5 servers spawn in <3s total at startup
- [ ] Per-request proxy overhead <5ms
