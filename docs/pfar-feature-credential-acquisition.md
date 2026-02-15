# PFAR Feature Spec: Credential Acquisition System

> **Feature**: Secure credential collection during integration setup — secrets never enter the LLM pipeline  
> **Status**: Implementation-ready  
> **Priority**: Phase 3 (blocks Dynamic Integrations)  
> **Depends on**: Vault (Phase 1), Gateway (Phase 1)

---

## 1. Problem

When the owner sets up a new integration ("Connect Notion"), the bot asks for an API token. The owner pastes `ntn_v2_abc123...` into Telegram. This breaks the pipeline:

- Intent extractor returns `None` (a raw token has no intent keywords)
- `should_use_full_pipeline()` returns `false` (no intent → fast path)
- Synthesizer runs with no context → generic "I can't process that"

But **fixing the pipeline is the wrong approach**. Sending a raw API token through Extract → Plan → Synthesize means an LLM sees the credential. Even if we special-cased the planner to call `store_credential`, the token would appear in the LLM's context window — logged by the cloud provider, cached in conversation history, vulnerable to prompt injection extraction.

The research confirms this is an industry-wide problem. OpenClaw sends credentials straight through the LLM pipeline in plaintext. Snyk found 7.1% of ClawHub skills pass API keys through the agent's context window. The `buy-anything` skill asked users for credit card details and tokenized them through Stripe within the agent's context. Bitsight found 30,000+ exposed OpenClaw instances leaking API keys.

The solution is that **credentials should never enter the pipeline at all**. The kernel intercepts them before Phase 0.

---

## 2. Design Principles

Three principles from the research, in priority order:

**P1: Access without exposure.** The agent should use credentials it never sees. This is 1Password's core principle for agentic AI — credentials are resolved at the execution boundary (kernel → vault → HTTP request), never in the LLM context.

**P2: Prefer out-of-band acquisition.** The safest credential is one that never appears in chat. OAuth flows, local web forms, and vault imports are all better than pasting a token into Telegram. In-chat paste is the fallback, not the default.

**P3: Process-then-delete.** When in-chat paste is unavoidable, minimize exposure: intercept before the pipeline, store in vault, delete the message from chat history immediately. The token exists in plaintext only for milliseconds.

---

## 3. Architecture

The Credential Acquisition System is a **kernel subsystem** that sits between the gateway and the pipeline. It intercepts messages before Phase 0 when a credential prompt is pending.

```
Gateway (Telegram) ─→ CredentialGate ─→ Pipeline (Phase 0 → 3)
                           │
                           ├─→ Vault (store secret)
                           ├─→ Gateway (delete message)
                           └─→ IntegrationSetup (continue flow)
```

Three acquisition methods, tried in preference order:

```
┌─────────────────────────────────────────────────────┐
│  Method 1: OAuth / Device Flow                       │
│  Service supports OAuth → user authenticates in      │
│  browser → kernel receives tokens via callback/poll  │
│  Token NEVER appears in chat. Best security.         │
├─────────────────────────────────────────────────────┤
│  Method 2: Local Web Form                            │
│  Kernel serves HTTPS on localhost → sends link in    │
│  chat → owner pastes token into password field →     │
│  form submits to localhost → kernel receives token   │
│  Token appears in browser, not in chat. Good.        │
├─────────────────────────────────────────────────────┤
│  Method 3: In-Chat Paste (Fallback)                  │
│  Owner pastes token in Telegram → kernel intercepts  │
│  before Phase 0 → stores in vault → deletes message  │
│  Token briefly in chat. Acceptable with mitigations. │
└─────────────────────────────────────────────────────┘
```

The built-in registry declares which method each service supports:

```rust
KnownServer {
    name: "github",
    // ...
    auth_methods: &[
        AuthMethod::OAuthDeviceFlow {
            device_auth_url: "https://github.com/login/device/code",
            token_url: "https://github.com/login/oauth/access_token",
            client_id: "PFAR_GITHUB_CLIENT_ID",  // PFAR's registered OAuth app
            scopes: &["repo", "read:org"],
        },
        AuthMethod::PasteToken {
            vault_key: "github_token",
            expected_prefix: Some("ghp_"),
            instructions: "github.com/settings/tokens → Fine-grained → Copy",
        },
    ],
}
```

The agent presents the best available method. If OAuth fails or the owner prefers paste, it falls back.

---

## 4. Method 1: OAuth Device Flow

For services that support OAuth (GitHub, Google, Slack, Microsoft, Spotify), this is the gold standard. The token never appears in chat.

### How it works

```
Owner: "Connect GitHub"
→ Agent: "I'll connect GitHub. Open this link and enter the code:

   🔗 https://github.com/login/device
   📋 Code: WDJB-MJHT

   I'll wait while you authorize."

→ Kernel polls GitHub's token endpoint every 5 seconds
→ Owner opens browser, enters code, authorizes PFAR
→ GitHub returns access token to kernel
→ Kernel stores token in vault
→ Agent: "GitHub connected. I can access repos, issues, and PRs."
```

### Implementation

```rust
pub struct DeviceFlowState {
    pub service: String,
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_at: Instant,
    pub interval: Duration,          // polling interval (default 5s)
    pub vault_key: String,
    pub principal_id: PrincipalId,
}

impl CredentialAcquisition {
    pub async fn start_device_flow(
        &self,
        config: &OAuthDeviceFlowConfig,
        principal: &PrincipalId,
    ) -> Result<DeviceFlowState> {
        // 1. Request device code from authorization server
        let resp = self.http_client.post(&config.device_auth_url)
            .form(&[
                ("client_id", &config.client_id),
                ("scope", &config.scopes.join(" ")),
            ])
            .send().await?;

        let device: DeviceAuthResponse = resp.json().await?;

        // 2. Create polling state
        let state = DeviceFlowState {
            service: config.service.clone(),
            device_code: device.device_code,
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri.clone(),
            expires_at: Instant::now() + Duration::from_secs(device.expires_in),
            interval: Duration::from_secs(device.interval.unwrap_or(5)),
            vault_key: config.vault_key.clone(),
            principal_id: principal.clone(),
        };

        // 3. Start background polling task
        self.spawn_device_poll(state.clone());

        Ok(state)
    }

    fn spawn_device_poll(&self, state: DeviceFlowState) {
        let vault = self.vault.clone();
        let setup = self.integration_setup.clone();
        let gateway = self.gateway.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(state.interval).await;

                if Instant::now() > state.expires_at {
                    gateway.send(&state.principal_id,
                        "Authorization timed out. Say 'connect {service}' to try again."
                    ).await;
                    return;
                }

                match poll_token_endpoint(&state).await {
                    Ok(TokenResponse { access_token, refresh_token, .. }) => {
                        // Store in vault — agent never sees this
                        vault.store_secret(&state.vault_key, &access_token).unwrap();
                        if let Some(refresh) = refresh_token {
                            vault.store_secret(
                                &format!("{}_refresh", state.vault_key),
                                &refresh,
                            ).unwrap();
                        }

                        // Continue integration setup
                        setup.complete(&state.service, &state.principal_id).await;

                        gateway.send(&state.principal_id,
                            &format!("{} connected successfully.", state.service)
                        ).await;
                        return;
                    }
                    Err(PollError::AuthorizationPending) => continue,
                    Err(PollError::SlowDown) => {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                    Err(PollError::AccessDenied) => {
                        gateway.send(&state.principal_id,
                            "Authorization was denied. Say 'connect {service}' to try again."
                        ).await;
                        return;
                    }
                    Err(e) => {
                        log::error!("Device flow error: {}", e);
                        gateway.send(&state.principal_id,
                            "Something went wrong with authorization. You can send me the token directly instead."
                        ).await;
                        // Fall back to paste method — register pending prompt
                        // (see Method 3)
                        return;
                    }
                }
            }
        });
    }
}
```

### Security properties

- Token **never appears in chat** — GitHub sends it directly to the kernel via HTTPS
- No callback URL needed — polling is outbound-only, works behind NAT/firewall
- Short-lived device codes expire (typically 15 minutes)
- Kernel stores token in vault immediately; polling task has no persistent access

### Security concern: Device code phishing

The research found active exploitation of device code flows. Secureworks documented PhishInSuits where attackers generate device codes and send them to victims. Mitigation:

- PFAR generates its own device codes — the owner initiates the flow, so they know they requested it
- The agent displays the code in the same conversation context where the owner asked for it
- For enterprise deployments, conditional access policies can restrict device flow (Microsoft already blocks it internally)

This is a social engineering risk, not a technical one. In PFAR's case, the owner initiates the flow themselves, so the risk is minimal.

### Which services support device flow

| Service | Device Flow | Notes |
|---------|-------------|-------|
| GitHub | ✅ | Full support via github.com/login/device |
| Microsoft/Azure | ✅ | microsoft.com/devicelogin (being restricted) |
| Google | ✅ | Limited device flow, prefers redirect |
| Slack | ❌ | OAuth redirect only |
| Notion | ❌ | Bearer token only |
| Linear | ❌ | OAuth redirect only |
| Most SaaS APIs | ❌ | Bearer token / API key only |

Reality: most services PFAR integrates with use simple API tokens, not OAuth. Device flow covers ~20% of integrations (the big ones). The other 80% need Methods 2 or 3.

---

## 5. Method 2: Local Web Form

For services that use API tokens (Notion, Linear, OpenAI, etc.), the owner needs to paste a token somewhere. A local web form is better than chat because:

- The form field has `type="password"` — the token is masked
- The token transits localhost only — never enters Telegram's servers
- No race condition with `deleteMessage` — the token was never in chat
- The form can validate the token format before accepting it

### How it works

```
Owner: "Connect Notion"
→ Agent: "I'll connect Notion. Open this link to enter your token securely:

   🔗 http://localhost:19275/credential/notion
   
   (Go to notion.so/profile/integrations first to create a token)
   
   Or just paste the token here if you prefer."

→ Owner clicks link, browser opens a minimal form
→ Owner pastes token into password field, clicks Submit
→ Form POSTs to localhost → kernel receives token → stores in vault
→ Agent: "Notion connected. I found 12 tools available."
```

### Implementation

```rust
pub struct CredentialWebServer {
    port: u16,                           // default 19275
    pending: Arc<Mutex<HashMap<String, PendingCredential>>>,
    vault: Arc<Vault>,
}

struct PendingCredential {
    service: String,
    vault_key: String,
    expected_prefix: Option<String>,
    principal_id: PrincipalId,
    nonce: String,                       // CSRF protection
    expires_at: Instant,
}

impl CredentialWebServer {
    pub fn start(vault: Arc<Vault>) -> Result<Self> {
        let server = Self {
            port: 19275,
            pending: Arc::new(Mutex::new(HashMap::new())),
            vault,
        };

        // Bind to localhost only — not accessible from network
        let listener = TcpListener::bind(format!("127.0.0.1:{}", server.port))?;

        tokio::spawn(async move {
            // Minimal HTTP server — two routes:
            // GET  /credential/:service  → render HTML form
            // POST /credential/:service  → receive token, store in vault
        });

        Ok(server)
    }

    pub fn register_pending(&self, service: &str, config: &PasteTokenConfig, 
                            principal: &PrincipalId) -> String {
        let nonce = generate_nonce();
        let url = format!("http://localhost:{}/credential/{}?n={}", 
                          self.port, service, nonce);

        self.pending.lock().unwrap().insert(
            format!("{}:{}", service, nonce),
            PendingCredential {
                service: service.to_string(),
                vault_key: config.vault_key.clone(),
                expected_prefix: config.expected_prefix.clone(),
                principal_id: principal.clone(),
                nonce: nonce.clone(),
                expires_at: Instant::now() + Duration::from_secs(600), // 10 min
            },
        );

        url
    }

    fn render_form(&self, service: &str, nonce: &str) -> String {
        // Minimal, self-contained HTML — no external resources
        format!(r#"<!DOCTYPE html>
<html><head>
<title>PFAR — Enter {service} Credential</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  body {{ font-family: system-ui; max-width: 400px; margin: 80px auto; 
         padding: 0 20px; color: #1a1a1a; }}
  h2 {{ margin-bottom: 4px; }}
  p {{ color: #666; margin-top: 4px; }}
  input[type=password] {{ width: 100%; padding: 12px; font-size: 16px; 
         border: 2px solid #ddd; border-radius: 6px; margin: 8px 0; 
         font-family: monospace; }}
  input[type=password]:focus {{ border-color: #0066cc; outline: none; }}
  button {{ width: 100%; padding: 12px; font-size: 16px; border: none; 
           border-radius: 6px; background: #0066cc; color: white; 
           cursor: pointer; }}
  button:hover {{ background: #0052a3; }}
  .ok {{ color: #2e7d32; text-align: center; }}
  .err {{ color: #c62828; font-size: 14px; }}
</style></head>
<body>
  <h2>🔐 {service}</h2>
  <p>Paste your API token below. It will be stored securely in PFAR's
     encrypted vault and never shown again.</p>
  <form method="POST" action="/credential/{service}">
    <input type="hidden" name="nonce" value="{nonce}">
    <input type="password" name="token" placeholder="Paste token here"
           autofocus required>
    <button type="submit">Store Credential</button>
  </form>
  <script>
    // Auto-close tab after successful submission
    document.querySelector('form').addEventListener('submit', async (e) => {{
      e.preventDefault();
      const form = e.target;
      const resp = await fetch(form.action, {{
        method: 'POST',
        body: new URLSearchParams(new FormData(form)),
      }});
      if (resp.ok) {{
        document.body.innerHTML = '<h2 class="ok">✓ Stored securely</h2>'
          + '<p>You can close this tab.</p>';
      }} else {{
        const err = await resp.text();
        form.querySelector('.err')?.remove();
        form.insertAdjacentHTML('beforeend', 
          '<p class="err">' + err + '</p>');
      }}
    }});
  </script>
</body></html>"#, service = service, nonce = nonce)
    }

    async fn handle_post(&self, service: &str, nonce: &str, token: &str) 
        -> Result<()> 
    {
        let key = format!("{}:{}", service, nonce);
        let pending = self.pending.lock().unwrap().remove(&key)
            .ok_or(Error::InvalidNonce)?;

        if Instant::now() > pending.expires_at {
            return Err(Error::Expired);
        }

        // Validate token format
        if let Some(prefix) = &pending.expected_prefix {
            if !token.starts_with(prefix) {
                return Err(Error::InvalidFormat(format!(
                    "Expected token starting with '{}'", prefix
                )));
            }
        }

        // Store in vault — this is the only place the token is handled
        self.vault.store_secret(&pending.vault_key, token)?;

        // Signal the integration setup to continue
        self.integration_setup.complete(
            &pending.service, &pending.principal_id
        ).await?;

        Ok(())
    }
}
```

### Security properties

- Token never enters Telegram — goes directly from browser to localhost
- `type="password"` masks input — no shoulder surfing
- CSRF nonce prevents other pages from submitting tokens
- Localhost-only binding — not accessible from network
- 10-minute expiry on pending credentials
- Form is self-contained HTML — no external JS, no CDN, no tracking

### Limitations

- Requires the owner to have browser access on the same machine running PFAR (or SSH tunnel). Works for desktop/laptop deployments. May not work for headless servers.
- If PFAR runs on a remote server, the owner needs SSH port forwarding: `ssh -L 19275:localhost:19275 server`. The agent can detect this and provide the command.
- Mobile-only users (Telegram on phone, PFAR on server) can't easily use this. They fall back to Method 3.

---

## 6. Method 3: In-Chat Paste with Kernel Intercept

The fallback for when OAuth isn't available and the local web form isn't accessible. This is the scenario described in the original bug report.

### The CredentialGate

The CredentialGate is a message filter that runs **before the pipeline**. It checks if there's a pending credential prompt for this principal. If so, it intercepts the message, stores the credential, and prevents it from entering Phase 0.

```rust
/// Sits between Gateway and Pipeline.
/// Intercepts credential replies before any LLM sees them.
pub struct CredentialGate {
    pending_prompts: HashMap<PrincipalId, PendingPrompt>,
    vault: Arc<Vault>,
    gateway: Arc<dyn Gateway>,
}

pub struct PendingPrompt {
    pub service: String,
    pub vault_key: String,
    pub expected_prefix: Option<String>,
    pub token_pattern: Option<Regex>,     // e.g., r"^ntn_[A-Za-z0-9_]{40,}$"
    pub prompted_at: Instant,
    pub ttl: Duration,                     // default 10 minutes
    pub message_id_of_prompt: Option<i64>, // so we can reference it
    pub web_form_url: Option<String>,      // if Method 2 was also offered
}

impl CredentialGate {
    /// Called by the kernel for every incoming message, before the pipeline.
    /// Returns true if the message was intercepted (pipeline should NOT run).
    pub async fn intercept(&mut self, msg: &IncomingMessage) -> bool {
        let Some(pending) = self.pending_prompts.get(&msg.principal_id) else {
            return false; // no pending prompt for this user
        };

        // Check TTL
        if pending.prompted_at.elapsed() > pending.ttl {
            self.pending_prompts.remove(&msg.principal_id);
            return false; // expired — let message through to pipeline
        }

        let text = msg.text.trim();

        // Classify: is this a credential or a normal message?
        match self.classify(text, pending) {
            Classification::Credential => {
                self.handle_credential(text, msg, pending).await;
                true  // intercepted — do NOT enter pipeline
            }
            Classification::Cancel => {
                self.pending_prompts.remove(&msg.principal_id);
                self.gateway.send(
                    &msg.principal_id,
                    "Integration setup cancelled.",
                ).await;
                true
            }
            Classification::NormalMessage => {
                false // not a credential — let pipeline handle it
            }
        }
    }

    fn classify(&self, text: &str, pending: &PendingPrompt) -> Classification {
        // Cancel commands
        let lower = text.to_lowercase();
        if lower == "cancel" || lower == "nevermind" || lower == "skip" {
            return Classification::Cancel;
        }

        // If there's a known prefix, check for it
        if let Some(prefix) = &pending.expected_prefix {
            if text.starts_with(prefix) {
                return Classification::Credential;
            }
        }

        // If there's a regex pattern, check for it
        if let Some(pattern) = &pending.token_pattern {
            if pattern.is_match(text) {
                return Classification::Credential;
            }
        }

        // Heuristic: looks like a token (no spaces, >20 chars, 
        // mostly alphanumeric + common token chars)
        if self.looks_like_token(text) {
            return Classification::Credential;
        }

        // Doesn't look like a credential — normal message
        Classification::NormalMessage
    }

    fn looks_like_token(&self, text: &str) -> bool {
        // Too short or too long
        if text.len() < 15 || text.len() > 500 { return false; }

        // Contains spaces or newlines — natural language
        if text.contains(' ') || text.contains('\n') { return false; }

        // High ratio of alphanumeric + token chars
        let token_chars = text.chars()
            .filter(|c| c.is_alphanumeric() || "-_.:=+/".contains(*c))
            .count();
        let ratio = token_chars as f64 / text.len() as f64;

        ratio > 0.9
    }

    async fn handle_credential(
        &mut self, 
        token: &str, 
        msg: &IncomingMessage,
        pending: &PendingPrompt,
    ) {
        // 1. Store in vault FIRST
        match self.vault.store_secret(&pending.vault_key, token) {
            Ok(_) => {
                log::info!(
                    "Stored credential for {} (vault_key: {})", 
                    pending.service, pending.vault_key
                );
            }
            Err(e) => {
                log::error!("Failed to store credential: {}", e);
                self.gateway.send(
                    &msg.principal_id,
                    "Failed to store credential. Please try again.",
                ).await;
                return;
            }
        }

        // 2. Delete the message containing the credential from chat
        //    This is the most time-critical step.
        if let Err(e) = self.gateway.delete_message(
            &msg.principal_id, msg.message_id
        ).await {
            log::warn!(
                "Could not delete credential message: {}. \
                 Token may remain in chat history.", e
            );
            // Not fatal — the credential is already safely in the vault.
            // But warn the owner.
            self.gateway.send(
                &msg.principal_id,
                "⚠️ I couldn't delete your message containing the token. \
                 You may want to delete it manually for security.",
            ).await;
        }

        // 3. Remove pending prompt
        let pending = self.pending_prompts.remove(&msg.principal_id).unwrap();

        // 4. Continue integration setup
        self.integration_setup.complete(
            &pending.service, &pending.principal_id
        ).await;

        // 5. Confirm (never echo the credential)
        self.gateway.send(
            &msg.principal_id,
            &format!("✓ {} credential stored securely. Setting up integration...", 
                     pending.service),
        ).await;
    }
}
```

### Gateway-level message deletion

The gateway adapter must support deleting messages. For Telegram:

```rust
impl TelegramGateway {
    pub async fn delete_message(
        &self, 
        principal: &PrincipalId, 
        message_id: i64,
    ) -> Result<()> {
        // Telegram Bot API: deleteMessage
        // Works in private chats, 48-hour window
        self.bot.delete_message(principal.chat_id, message_id).await?;
        Ok(())
    }

    pub async fn delete_messages_batch(
        &self,
        principal: &PrincipalId,
        message_ids: &[i64],
    ) -> Result<()> {
        // Telegram Bot API: deleteMessages (batch, up to 100)
        self.bot.delete_messages(principal.chat_id, message_ids).await?;
        Ok(())
    }
}
```

### Token pattern registry

Known token formats for reliable classification:

```rust
const TOKEN_PATTERNS: &[(&str, &str, &str)] = &[
    // (service, prefix, regex)
    ("notion",    "ntn_",   r"^ntn_[A-Za-z0-9]{40,}$"),
    ("github",    "ghp_",   r"^gh[ps]_[A-Za-z0-9]{36,}$"),
    ("github",    "github_pat_", r"^github_pat_[A-Za-z0-9_]{80,}$"),
    ("openai",    "sk-",    r"^sk-[A-Za-z0-9-_]{40,}$"),
    ("slack",     "xoxb-",  r"^xox[bpra]-[A-Za-z0-9-]+$"),
    ("anthropic", "sk-ant-", r"^sk-ant-[A-Za-z0-9-_]{80,}$"),
    ("linear",    "lin_api_", r"^lin_api_[A-Za-z0-9]{40,}$"),
    ("stripe",    "sk_",    r"^sk_(live|test)_[A-Za-z0-9]{24,}$"),
    ("sendgrid",  "SG.",    r"^SG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43}$"),
    ("telegram",  "",       r"^\d{8,10}:[A-Za-z0-9_-]{35}$"),
];
```

### Security properties

- Token intercepted **before Phase 0** — no LLM ever sees it
- `deleteMessage` called immediately after vault storage — minimizes exposure window
- Token never echoed back in any response
- 10-minute TTL on pending prompts — stale prompts don't intercept random messages
- If deletion fails, owner is warned to delete manually

### Limitations

- Token briefly exists in Telegram's servers (in transit + until deletion)
- `deleteMessage` has a 48-hour window — if the bot crashes before deletion, the token stays in chat. Mitigation: store a "pending deletion" record so the kernel retries on restart.
- Telegram notifications on the owner's phone may have already shown the token. Deletion removes it from chat but not from notification history. No mitigation possible.
- If the owner sends the token in a group chat, other members may have already seen it. Mitigation: credential prompts should warn "send this in our private chat."

---

## 7. Orchestration: The Full Flow

The `CredentialAcquisition` subsystem coordinates all three methods:

```rust
pub struct CredentialAcquisition {
    gate: CredentialGate,           // Method 3: in-chat intercept
    web_server: CredentialWebServer, // Method 2: local web form
    device_flows: DeviceFlowManager, // Method 1: OAuth device flow
    vault: Arc<Vault>,
    gateway: Arc<dyn Gateway>,
    integration_setup: Arc<IntegrationSetup>,
}

impl CredentialAcquisition {
    /// Called when the pipeline determines a credential is needed.
    /// Decides the best acquisition method and initiates it.
    pub async fn request_credential(
        &mut self,
        service: &str,
        auth_methods: &[AuthMethod],
        principal: &PrincipalId,
    ) -> Result<()> {
        // Try methods in preference order
        for method in auth_methods {
            match method {
                AuthMethod::OAuthDeviceFlow { .. } => {
                    match self.try_device_flow(method, principal).await {
                        Ok(state) => {
                            // Send device code to owner
                            self.gateway.send(principal, &format!(
                                "I'll connect {service}. Open this link and enter the code:\n\n\
                                 🔗 {uri}\n\
                                 📋 Code: `{code}`\n\n\
                                 I'll wait while you authorize.",
                                service = service,
                                uri = state.verification_uri,
                                code = state.user_code,
                            )).await;
                            return Ok(());
                        }
                        Err(e) => {
                            log::warn!("Device flow failed for {}: {}", service, e);
                            continue; // try next method
                        }
                    }
                }
                AuthMethod::PasteToken { vault_key, expected_prefix, instructions } => {
                    // Register web form (Method 2)
                    let web_url = self.web_server.register_pending(
                        service, vault_key, expected_prefix.as_deref(), principal,
                    );

                    // Register in-chat intercept (Method 3)
                    self.gate.register_pending(PendingPrompt {
                        service: service.to_string(),
                        vault_key: vault_key.clone(),
                        expected_prefix: expected_prefix.clone(),
                        token_pattern: lookup_token_pattern(service),
                        prompted_at: Instant::now(),
                        ttl: Duration::from_secs(600),
                        message_id_of_prompt: None,
                        web_form_url: Some(web_url.clone()),
                    }, principal);

                    // Send prompt offering both Method 2 and 3
                    self.gateway.send(principal, &format!(
                        "To connect {service}, I need an API token.\n\n\
                         {instructions}\n\n\
                         🔗 Enter it securely: {web_url}\n\
                         Or just paste it here — I'll delete the message immediately.",
                        service = service,
                        instructions = instructions,
                        web_url = web_url,
                    )).await;

                    return Ok(());
                }
                AuthMethod::OAuthCallback { .. } => {
                    // Future: for services requiring redirect-based OAuth
                    // Kernel runs temporary callback server
                    continue;
                }
            }
        }

        Err(Error::NoAuthMethodAvailable(service.to_string()))
    }
}
```

### Integration with admin tools

The `admin.connect_service` tool is what the Planner calls. It doesn't handle credentials — it delegates to `CredentialAcquisition`:

```rust
pub fn execute_connect_service(
    &self, 
    args: &ConnectServiceArgs,
    principal: &PrincipalId,
) -> ToolOutput {
    // Look up service in registry
    let service = self.registry.find(&args.service_name)?;

    // Check if already connected
    if self.vault.has_secret(&service.default_vault_key)? {
        return ToolOutput::text(format!(
            "{} is already connected.", service.name
        ));
    }

    // Delegate to CredentialAcquisition
    self.credential_acquisition.request_credential(
        &service.name,
        &service.auth_methods,
        principal,
    ).await?;

    // Return "waiting for credential" status — the pipeline is done
    // for this turn. Next message will either be intercepted by
    // CredentialGate or the web form will complete.
    ToolOutput::text(format!(
        "Waiting for {} credentials from owner.", service.name
    ))
}
```

---

## 8. Pending Deletion Recovery

If the kernel crashes between storing the credential and deleting the chat message, the token remains in chat history. To handle this:

```rust
// On credential intercept, before attempting deletion:
self.pending_deletions.insert(PendingDeletion {
    principal_id: msg.principal_id.clone(),
    message_id: msg.message_id,
    created_at: Instant::now(),
});

// Persist to disk (simple append-only log)
self.write_pending_deletion_log(&msg.principal_id, msg.message_id)?;

// ... attempt deletion ...

// On success, remove from log
self.clear_pending_deletion(&msg.principal_id, msg.message_id)?;

// On kernel startup:
pub fn recover_pending_deletions(&mut self) {
    for entry in self.read_pending_deletion_log() {
        if let Err(e) = self.gateway.delete_message(
            &entry.principal_id, entry.message_id
        ).await {
            log::warn!("Recovery: could not delete message {}: {}", 
                       entry.message_id, e);
            // If >48 hours old, Telegram won't allow deletion.
            // Log and move on.
        }
    }
}
```

---

## 9. Edge Cases

### Owner sends normal message while credential is pending

The `classify()` function distinguishes credentials from natural language. If the message has spaces, is short, or matches no token pattern, it goes to the pipeline. The pending prompt remains active.

```
Owner: "Connect Notion"
→ Bot: "I need your Notion token. Paste it here or use the link: ..."

Owner: "Wait, what's the weather today?"
→ classify() → NormalMessage (has spaces, no token pattern)
→ Pipeline handles it normally
→ Pending prompt still active

Owner: "ntn_v2_abc123..."
→ classify() → Credential (matches ntn_ prefix)
→ Intercepted, stored, deleted
```

### Owner says "cancel" during credential prompt

```
Owner: "cancel"
→ classify() → Cancel
→ Pending prompt removed
→ "Integration setup cancelled."
```

### Owner pastes token without being asked

No pending prompt → `intercept()` returns `false` → message goes to pipeline → Extractor sees gibberish → fast path → generic response. This is fine — we don't want to intercept random messages that happen to look like tokens.

### Multiple services pending simultaneously

The `pending_prompts` map is keyed by `PrincipalId`. Only one pending prompt per principal. If the owner starts connecting a second service before finishing the first, the old prompt is replaced.

### Token fails validation after storage

After storing and spawning the MCP server (or writing the manifest), the kernel makes a test API call. If it fails:

```
→ "The Notion token doesn't seem to work — I got a 401 Unauthorized.
    Want to try a different token? Or say 'cancel' to stop."
→ Re-register pending prompt (new TTL)
→ Delete the failed credential from vault
```

### Owner on mobile, PFAR on remote server

Method 2 (web form) won't work without SSH tunnel. The agent should detect this:

```rust
fn is_local_access_likely(principal: &PrincipalId) -> bool {
    // Heuristic: if the gateway is Telegram, the owner is likely remote
    // Could also check if web server got a health-check hit recently
    matches!(principal.channel, Channel::Telegram | Channel::WhatsApp)
}
```

If local access is unlikely, skip offering the web form link and go straight to in-chat paste with clear instructions about message deletion.

---

## 10. Audit Trail

Every credential operation is logged (the credential value is never logged):

```rust
pub struct CredentialAuditEntry {
    pub timestamp: DateTime<Utc>,
    pub principal: PrincipalId,
    pub service: String,
    pub vault_key: String,
    pub method: AcquisitionMethod,       // DeviceFlow | WebForm | ChatPaste
    pub chat_message_deleted: bool,       // for ChatPaste only
    pub token_prefix: String,             // first 4 chars only, for debugging
}
```

Example log:
```
2026-02-14T18:30:00Z principal=owner service=notion vault_key=notion_token 
  method=ChatPaste deleted=true prefix=ntn_
2026-02-14T18:45:00Z principal=owner service=github vault_key=github_token 
  method=DeviceFlow prefix=ghp_
```

---

## 11. Security Summary

| Threat | Method 1 (OAuth) | Method 2 (Web Form) | Method 3 (Chat Paste) |
|--------|-------------------|----------------------|------------------------|
| LLM sees credential | Impossible | Impossible | Impossible (kernel intercept) |
| Credential in chat history | Never in chat | Never in chat | Brief — deleted immediately |
| Credential in Telegram servers | Never | Never | Brief — deleted via API |
| Credential in notification history | Never | Never | Possible (no mitigation) |
| Credential in cloud LLM logs | Never | Never | Never |
| Shoulder surfing | N/A | Masked (password field) | Visible briefly |
| Crash before cleanup | Token already in vault | Token already in vault | Token in chat until recovery |
| MITM on credential transport | HTTPS to OAuth provider | localhost only | Telegram TLS |

### Compared to the alternatives

**vs. OpenClaw**: OpenClaw sends credentials through the LLM pipeline in plaintext. 7.1% of skills pass API keys through the agent's context. PFAR's CredentialGate ensures no LLM ever sees a credential, regardless of acquisition method.

**vs. "just fix the pipeline"**: Routing credentials through Extract → Plan → store_credential means the Extractor LLM, Planner LLM, and potentially Synthesizer LLM all see the raw token. Even if we added `[REDACTED]` heuristics, LLM context windows are logged by providers, cached in conversation history, and vulnerable to prompt injection. The kernel intercept eliminates this entire class of risk.

**vs. "require config file editing"**: Asking users to manually edit TOML files and paste tokens there defeats the "conversational setup" goal. The web form provides the security of file-based entry with the convenience of a guided flow.

---

## 12. Implementation Checklist

### CredentialGate (kernel intercept) — critical path
- [ ] `CredentialGate` struct with `intercept()` method
- [ ] `PendingPrompt` with TTL, expected prefix, regex pattern
- [ ] `classify()` — credential vs cancel vs normal message
- [ ] `looks_like_token()` heuristic
- [ ] Token pattern registry for known services
- [ ] Gateway `delete_message()` / `delete_messages_batch()`
- [ ] Pending deletion recovery on kernel restart
- [ ] Audit logging for all credential operations

### Local web form
- [ ] `CredentialWebServer` bound to localhost only
- [ ] GET route renders HTML form with CSRF nonce
- [ ] POST route validates nonce + token format, stores in vault
- [ ] Self-contained HTML (no external resources)
- [ ] 10-minute expiry on pending credentials
- [ ] Signal integration setup on successful submission

### OAuth device flow
- [ ] `DeviceFlowManager` with background polling
- [ ] Device code request to authorization server
- [ ] Polling with backoff (honor `slow_down` response)
- [ ] Token storage in vault on success
- [ ] Expiry handling (re-prompt on timeout)
- [ ] Graceful fallback to paste method on failure

### Orchestration
- [ ] `CredentialAcquisition.request_credential()` tries methods in order
- [ ] `admin.connect_service` delegates to CredentialAcquisition
- [ ] Built-in registry with `auth_methods` per service
- [ ] Detect local vs remote access for web form availability

### Tests
- [ ] Token pasted in chat → intercepted before Phase 0, stored, deleted
- [ ] Normal message during pending prompt → passes through to pipeline
- [ ] "cancel" during pending prompt → prompt cleared
- [ ] TTL expiry → pending prompt removed, message passes through
- [ ] Token with wrong prefix → rejected, owner re-prompted
- [ ] Web form → token stored, integration setup continues
- [ ] Web form with expired nonce → rejected
- [ ] OAuth device flow → token received via polling, stored
- [ ] OAuth timeout → owner re-prompted
- [ ] Kernel crash recovery → pending deletions retried
- [ ] Audit log records method, service, success/failure (never the token)
- [ ] No LLM context window contains credential text (integration test)
