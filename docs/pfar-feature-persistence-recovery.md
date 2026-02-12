# PFAR Feature Spec: Persistence & Recovery

> **Feature**: Runtime State Persistence and Crash Recovery  
> **Status**: Implementation-ready  
> **Depends on**: Vault (sessions.db), Kernel Core, Adapters  
> **Priority**: Must-have (Phase 1/2)

---

## 1. Problem Statement

The PFAR kernel holds runtime state in memory that is lost on process crash or restart: in-flight tasks, pending approvals, plan execution progress, and credential prompt conversations. Without persistence, any interruption — kernel crash, host reboot, OOM kill, manual restart for update — causes silent data loss. The user sees no response to their request and has no indication of what happened.

---

## 2. Design Principles

1. **Journal before act**: Every state transition is written to disk before the kernel proceeds. If power is cut between any two operations, recovery can reconstruct what happened.
2. **Idempotent recovery**: Retrying a recovered task must not produce duplicate side effects (no double emails, no duplicate calendar events).
3. **Fail loud**: After restart, the owner is told what was lost or recovered. No silent drops.
4. **Bounded recovery window**: Tasks older than a configurable max age are abandoned, not retried. Stale retries are worse than clean failures.
5. **Vault is the single source of truth**: All persistent state lives in the existing SQLCipher databases. No new storage backends.

---

## 3. What Persists Where

| Data | Storage | Survives restart? | Notes |
|---|---|---|---|
| Secrets (API keys, tokens) | `secrets.db` | Yes | Already in spec |
| Conversation history | `sessions.db` | Yes | Already in spec |
| Session working memory | `sessions.db` | Yes | Already in spec |
| Long-term memory | `memory.db` | Yes | Already in spec |
| **Task journal** | `sessions.db` | **Yes (new)** | This feature |
| **Pending approvals** | `sessions.db` | **Yes (new)** | This feature |
| **Pending credential prompts** | `sessions.db` | **Yes (new)** | This feature |
| Circuit breaker state | Memory only | No | Intentional — reset to healthy on restart |
| Adapter connections | Memory only | No | Reconnect on startup |
| Container leases | Memory only | No | Reconciliation loop cleans orphans |
| LLM inference in progress | Memory only | No | Retry from phase start |
| Cron job definitions | Config files (TOML) | Yes | Already in spec |

---

## 4. Task Journal

### 4.1 Schema

New table in `sessions.db`:

```sql
CREATE TABLE task_journal (
    task_id         TEXT PRIMARY KEY,           -- UUID
    template_id     TEXT NOT NULL,
    principal       TEXT NOT NULL,              -- serialized Principal
    trigger_event   TEXT,                       -- JSON of original event (redacted)
    state           TEXT NOT NULL,              -- enum: see below
    phase           TEXT NOT NULL,              -- "extract", "plan", "execute", "synthesize", "egress"
    plan_json       TEXT,                       -- serialized plan from Phase 1
    execute_progress TEXT,                      -- JSON: which steps completed, results so far
    extracted_metadata TEXT,                    -- JSON: Phase 0 output
    data_ceiling    TEXT NOT NULL,
    output_sinks    TEXT NOT NULL,              -- JSON array
    trace_id        TEXT,
    created_at      TEXT NOT NULL,              -- RFC3339
    updated_at      TEXT NOT NULL,              -- RFC3339
    error           TEXT                        -- failure reason if failed
);

CREATE INDEX idx_task_journal_state ON task_journal(state);
CREATE INDEX idx_task_journal_updated ON task_journal(updated_at);
```

### 4.2 Task States

```rust
pub enum PersistedTaskState {
    /// Phase 0 started but not completed
    Extracting,
    /// Phase 0 done, Phase 1 (plan) started
    Planning,
    /// Phase 1 done, plan saved, executing steps
    Executing {
        current_step: usize,
        completed_steps: Vec<CompletedStep>,
    },
    /// All steps done, Phase 3 started
    Synthesizing,
    /// Blocked on human approval
    AwaitingApproval {
        approval_id: Uuid,
        step: usize,
    },
    /// Blocked on credential input from owner
    AwaitingCredential {
        service: String,
        prompt_message_id: Option<String>,
    },
    /// Task completed successfully — eligible for cleanup
    Completed,
    /// Task failed — kept for audit, eligible for cleanup
    Failed,
    /// Task abandoned after restart (too old)
    Abandoned,
}

pub struct CompletedStep {
    pub step: usize,
    pub tool: String,
    pub action_semantics: Semantics,  // Read or Write
    pub result_json: serde_json::Value,
    pub label: SecurityLabel,
    pub completed_at: DateTime<Utc>,
}
```

### 4.3 Journal Write Points

The kernel writes to the task journal at every phase boundary — NOT on every micro-operation. This keeps I/O overhead minimal.

```
Event received
  │
  ├─> CREATE task_journal row (state: Extracting)
  │
  ├─> Phase 0 complete
  │   └─> UPDATE state=Planning, extracted_metadata=JSON
  │
  ├─> Phase 1 complete
  │   └─> UPDATE state=Executing{step:0}, plan_json=JSON
  │
  ├─> Each step completion
  │   └─> UPDATE execute_progress (append completed step)
  │
  ├─> If approval needed
  │   └─> UPDATE state=AwaitingApproval{approval_id, step}
  │
  ├─> If credential needed
  │   └─> UPDATE state=AwaitingCredential{service}
  │
  ├─> All steps complete
  │   └─> UPDATE state=Synthesizing
  │
  ├─> Phase 3 complete + egress done
  │   └─> UPDATE state=Completed
  │
  └─> On any failure
      └─> UPDATE state=Failed, error=reason
```

### 4.4 Journal Cleanup

Completed and failed tasks are retained in the journal for audit purposes, then cleaned up on a schedule:

```rust
const JOURNAL_RETENTION_COMPLETED: Duration = Duration::from_secs(24 * 3600);  // 24 hours
const JOURNAL_RETENTION_FAILED: Duration = Duration::from_secs(7 * 24 * 3600); // 7 days
const JOURNAL_RETENTION_ABANDONED: Duration = Duration::from_secs(7 * 24 * 3600);
```

Cleanup runs daily (can piggyback on vault backup schedule):
```sql
DELETE FROM task_journal 
WHERE state = 'Completed' AND updated_at < datetime('now', '-1 day');

DELETE FROM task_journal 
WHERE state IN ('Failed', 'Abandoned') AND updated_at < datetime('now', '-7 days');
```

---

## 5. Pending Approvals Persistence

### 5.1 Schema

New table in `sessions.db`:

```sql
CREATE TABLE pending_approvals (
    approval_id     TEXT PRIMARY KEY,          -- UUID
    task_id         TEXT NOT NULL REFERENCES task_journal(task_id),
    action_type     TEXT NOT NULL,             -- "tainted_write", "declassification", "identity_link", "cloud_routing"
    description     TEXT NOT NULL,             -- human-readable action description
    data_preview    TEXT,                      -- redacted preview of what will be written
    taint_level     TEXT,                      -- "Raw", "Extracted"
    target_sink     TEXT,                      -- where the write would go
    tool            TEXT,                      -- which tool is requesting
    step            INTEGER,                   -- which plan step
    created_at      TEXT NOT NULL,             -- RFC3339
    expires_at      TEXT NOT NULL,             -- RFC3339
    status          TEXT NOT NULL DEFAULT 'pending'  -- "pending", "approved", "denied", "expired", "recovered"
);

CREATE INDEX idx_pending_approvals_status ON pending_approvals(status);
```

### 5.2 Lifecycle

```
Tool write with taint detected
  │
  ├─> INSERT pending_approvals (status=pending)
  ├─> UPDATE task_journal (state=AwaitingApproval)
  ├─> Send approval message to admin sink (Telegram inline buttons)
  │
  ├─> Owner taps Approve
  │   ├─> UPDATE pending_approvals (status=approved)
  │   ├─> Resume task execution
  │   └─> UPDATE task_journal (state=Executing, next step)
  │
  ├─> Owner taps Deny
  │   ├─> UPDATE pending_approvals (status=denied)
  │   ├─> Skip step or fail task (per template config)
  │   └─> UPDATE task_journal accordingly
  │
  └─> Timeout (approval_timeout_seconds)
      ├─> UPDATE pending_approvals (status=expired)
      └─> UPDATE task_journal (state=Failed, error="approval timeout")
```

---

## 6. Pending Credential Prompts Persistence

### 6.1 Schema

New table in `sessions.db`:

```sql
CREATE TABLE pending_credentials (
    prompt_id       TEXT PRIMARY KEY,          -- UUID
    task_id         TEXT NOT NULL REFERENCES task_journal(task_id),
    service         TEXT NOT NULL,             -- "notion", "github", etc.
    credential_type TEXT NOT NULL,             -- "integration_token", "oauth", "api_key", "app_password"
    instructions    TEXT NOT NULL,             -- setup instructions shown to owner
    vault_ref       TEXT NOT NULL,             -- where to store: "vault:notion_token"
    message_id      TEXT,                      -- adapter message ID (for reply matching)
    created_at      TEXT NOT NULL,             -- RFC3339
    expires_at      TEXT NOT NULL,             -- RFC3339
    status          TEXT NOT NULL DEFAULT 'pending'  -- "pending", "received", "expired"
);
```

### 6.2 Lifecycle

```
admin.prompt_credential executes
  │
  ├─> INSERT pending_credentials (status=pending)
  ├─> UPDATE task_journal (state=AwaitingCredential)
  ├─> Send credential request message to admin sink
  │
  ├─> Owner replies with token/key
  │   ├─> Kernel matches reply to pending prompt (by conversation context)
  │   ├─> Store credential in vault (secrets.db)
  │   ├─> UPDATE pending_credentials (status=received)
  │   ├─> Resume task execution
  │   └─> UPDATE task_journal (state=Executing, next step)
  │
  └─> Timeout (credential_prompt_timeout, default 10 min)
      ├─> UPDATE pending_credentials (status=expired)
      └─> UPDATE task_journal (state=Failed, error="credential prompt timeout")
```

---

## 7. Startup and Recovery Sequence

### 7.1 Full Startup Flow

```rust
pub async fn startup(&self) -> Result<()> {
    // ── Stage 1: Foundation ──────────────────────────
    
    // 1. Load config from disk
    let config = Config::load_from_disk()?;
    
    // 2. Unlock vault databases
    //    Master key retrieved from OS keychain (macOS Keychain / Linux Secret Service)
    //    If keychain is locked (e.g., after reboot), prompt owner via stderr/CLI
    self.vault.unlock(&config.vault)?;
    
    // 3. Start audit logger (must be running before any actions)
    self.audit_logger.start(&config.observability)?;
    self.audit_logger.log(AuditEvent::SystemStartup {
        version: env!("CARGO_PKG_VERSION"),
        config_hash: config.content_hash(),
    });
    
    // ── Stage 2: Recovery ────────────────────────────
    
    // 4. Run container reconciliation immediately
    //    Kill any orphaned browser/script containers from before crash
    let orphans = self.container_manager.reconcile_now().await?;
    if orphans > 0 {
        self.audit_logger.log(AuditEvent::OrphanContainersKilled { count: orphans });
    }
    
    // 5. Recover incomplete tasks from journal
    let recovery_report = self.recover_tasks().await?;
    
    // ── Stage 3: Services ────────────────────────────
    
    // 6. Initialize policy engine with config
    self.policy_engine.load(&config.data_flow)?;
    
    // 7. Initialize tool registry (activate tools that have credentials)
    self.tool_registry.load(&config)?;
    
    // 8. Initialize template registry
    self.template_registry.load_from_dir(&config.templates_dir)?;
    
    // 9. Start inference proxy
    self.inference_proxy.start(&config.llm).await?;
    
    // 10. Start scheduler (cron jobs — won't fire until adapters are up)
    self.scheduler.start(&config)?;
    
    // ── Stage 4: Adapters ────────────────────────────
    
    // 11. Start adapters (each as async task)
    //     Order: CLI first (always works), then Telegram (primary),
    //     then others
    self.start_adapter_cli().await?;
    
    if config.adapter.telegram.enabled {
        match self.start_adapter_telegram(&config).await {
            Ok(_) => {},
            Err(e) => {
                // Telegram is primary channel — this is a critical failure
                // Log and continue (other adapters may work)
                self.audit_logger.log(AuditEvent::AdapterStartFailed {
                    adapter: "telegram",
                    error: e.to_string(),
                });
            }
        }
    }
    // ... repeat for slack, whatsapp, webhooks
    
    // 12. Start container reconciliation loop (periodic, every 30s)
    self.container_manager.start_reconciliation_loop();
    
    // ── Stage 5: Notify ──────────────────────────────
    
    // 13. Send recovery report to owner
    self.notify_owner_startup(&recovery_report).await?;
    
    Ok(())
}
```

### 7.2 Task Recovery Logic

```rust
pub async fn recover_tasks(&self) -> Result<RecoveryReport> {
    let mut report = RecoveryReport::default();
    let max_age = Duration::from_secs(self.config.recovery_max_age_seconds); // default: 600 (10 min)
    let now = Utc::now();
    
    // Load all non-terminal tasks from journal
    let incomplete = self.vault.sessions_db.query(
        "SELECT * FROM task_journal WHERE state NOT IN ('Completed', 'Failed', 'Abandoned')"
    )?;
    
    for task in incomplete {
        let age = now - task.updated_at;
        
        if age > max_age {
            // Too old — abandon
            self.abandon_task(&task, "Abandoned after restart: task exceeded max recovery age").await?;
            report.abandoned.push(task.task_id);
            continue;
        }
        
        let action = self.determine_recovery_action(&task);
        
        match action {
            RecoveryAction::RetryFromScratch => {
                // Task was in Extract or Plan phase — no side effects yet
                // Safe to retry the entire task
                self.retry_task_from_event(&task).await?;
                report.retried.push(task.task_id);
            }
            
            RecoveryAction::ResumeExecution => {
                // Task was mid-Execute — some steps completed
                // Resume from next incomplete step
                // Completed read steps: skip (results are in journal)
                // Completed write steps: skip (already executed)
                // Current step was in progress: check idempotency
                self.resume_task_execution(&task).await?;
                report.resumed.push(task.task_id);
            }
            
            RecoveryAction::Resynthesize => {
                // All tool steps completed, synthesis didn't finish
                // Tool results are in journal — just re-run Phase 3
                self.resynthesize_task(&task).await?;
                report.resumed.push(task.task_id);
            }
            
            RecoveryAction::RepromptApproval => {
                // Was waiting for human approval
                // Re-send the approval message (previous buttons are dead)
                self.reprompt_approval(&task).await?;
                report.reprompted.push(task.task_id);
            }
            
            RecoveryAction::RepromptCredential => {
                // Was waiting for credential input
                // Re-send the credential prompt
                self.reprompt_credential(&task).await?;
                report.reprompted.push(task.task_id);
            }
            
            RecoveryAction::Abandon => {
                self.abandon_task(&task, "Abandoned: unrecoverable state").await?;
                report.abandoned.push(task.task_id);
            }
        }
    }
    
    report
}

fn determine_recovery_action(&self, task: &PersistedTask) -> RecoveryAction {
    match &task.state {
        PersistedTaskState::Extracting => RecoveryAction::RetryFromScratch,
        PersistedTaskState::Planning => RecoveryAction::RetryFromScratch,
        
        PersistedTaskState::Executing { current_step, completed_steps } => {
            if completed_steps.is_empty() {
                // No steps completed — safe to retry from plan
                RecoveryAction::RetryFromScratch
            } else {
                RecoveryAction::ResumeExecution
            }
        }
        
        PersistedTaskState::Synthesizing => RecoveryAction::Resynthesize,
        
        PersistedTaskState::AwaitingApproval { .. } => {
            RecoveryAction::RepromptApproval
        }
        
        PersistedTaskState::AwaitingCredential { .. } => {
            RecoveryAction::RepromptCredential
        }
        
        // Terminal states should not appear in recovery
        _ => RecoveryAction::Abandon,
    }
}
```

### 7.3 Idempotency Rules for Step Recovery

When resuming mid-execution, the kernel must avoid double-executing side effects:

```rust
fn should_retry_step(&self, step: &CompletedStep, was_in_progress: bool) -> StepRecovery {
    match step.action_semantics {
        Semantics::Read => {
            // Reads are always safe to retry
            // But if we already have the result in the journal, skip
            if step.has_result() {
                StepRecovery::SkipWithCachedResult
            } else {
                StepRecovery::Retry
            }
        }
        
        Semantics::Write => {
            if was_in_progress {
                // Write was in progress when we crashed
                // We don't know if it completed on the remote side
                // Ask owner via approval queue
                StepRecovery::RequireOwnerConfirmation {
                    message: format!(
                        "I was interrupted while executing '{}'. \
                         It may have already completed. Should I retry it?",
                        step.tool
                    ),
                }
            } else {
                // Write hadn't started yet — safe to execute
                StepRecovery::Execute
            }
        }
    }
}

enum StepRecovery {
    /// Step already completed, use cached result from journal
    SkipWithCachedResult,
    /// Safe to retry (reads, or writes that hadn't started)
    Retry,
    /// Execute normally (write that hadn't started)
    Execute,
    /// Need owner to confirm whether to retry (write was in progress)
    RequireOwnerConfirmation { message: String },
}
```

### 7.4 Owner Startup Notification

After recovery, the owner gets a single summary message:

```rust
pub struct RecoveryReport {
    pub retried: Vec<Uuid>,       // tasks retried from scratch
    pub resumed: Vec<Uuid>,       // tasks resumed mid-execution
    pub reprompted: Vec<Uuid>,    // approvals/credentials re-sent
    pub abandoned: Vec<Uuid>,     // tasks too old, dropped
    pub orphan_containers: usize, // browser/script containers killed
}

// Message sent to admin sink:
// 
// "System restarted. Recovery report:
//  - 2 tasks retried (email check, GitHub digest)
//  - 1 task resumed (Notion page creation — step 2 of 3)
//  - 1 approval re-sent (Fireflies summary → Notion)
//  - 1 task abandoned (too old: WhatsApp scheduling from 25 min ago)
//  - 3 orphaned containers cleaned up"
//
// Or if clean restart:
// "System restarted. No pending tasks to recover."
```

---

## 8. Adapter Reconnection

Adapters are in-process async tasks. On startup they reconnect using credentials from the vault.

### 8.1 Per-Adapter Behavior

```rust
pub struct AdapterRecoveryConfig {
    /// Max reconnection attempts before giving up
    pub max_retries: u32,           // default: 5
    /// Backoff between retries
    pub retry_backoff: Duration,    // default: 5s, exponential up to 60s
    /// Whether missing messages are acceptable
    pub tolerates_gaps: bool,
}
```

| Adapter | Reconnection | Message gap? | Special handling |
|---|---|---|---|
| **Telegram** | Start polling from current offset. Telegram server buffers unacknowledged updates for 24h. | No gap if downtime < 24h | If offset was persisted, resume from last ack'd. If not, use `-1` (latest only) and accept gap. |
| **Slack** | Reconnect WebSocket via Socket Mode. Slack buffers events briefly (~30s). | Possible gap if downtime > 30s | On reconnect, optionally call `conversations.history` for monitored channels to catch up. |
| **WhatsApp** | Reload Baileys session from vault. Attempt reconnect. | Possible gap. WhatsApp may deliver missed messages on reconnect. | If session expired during downtime: mark adapter as degraded, notify owner to re-pair via QR. |
| **Webhooks** | Restart HTTP listener. No state needed. | External services may retry (most webhooks retry on 5xx/timeout). | If downtime was brief, webhook retries will deliver missed events. If long, events may be lost. |
| **CLI** | Always available (stdin/stdout). | No gap | — |

### 8.2 Telegram Offset Persistence

Telegram's `getUpdates` API uses an `offset` parameter. To avoid reprocessing old messages on restart:

```sql
-- In sessions.db
CREATE TABLE adapter_state (
    adapter     TEXT PRIMARY KEY,
    state_json  TEXT NOT NULL,      -- adapter-specific state
    updated_at  TEXT NOT NULL
);

-- Telegram stores:  {"last_offset": 123456789}
-- Slack stores:     {} (stateless reconnect)
-- WhatsApp stores:  {} (session is in secrets.db)
```

The Telegram adapter writes `last_offset` after successfully processing each batch of updates. On restart, it resumes from `last_offset + 1`.

### 8.3 Adapter Startup Order

Adapters start in priority order — primary channels first:

```
1. CLI         (instant, always works)
2. Telegram    (primary owner channel, needed for notifications)
3. Webhooks    (needed for incoming cron-triggered events)
4. Slack       (can tolerate delayed start)
5. WhatsApp    (most fragile, may need manual re-pairing)
```

If Telegram fails to connect, the kernel logs a critical alert and continues. Recovery notifications fall back to CLI output. If all adapters fail, the kernel continues running (cron jobs still work internally) and retries adapters on a backoff schedule.

---

## 9. Graceful Shutdown

On `SIGTERM` or `SIGINT`, the kernel should shut down cleanly:

```rust
pub async fn graceful_shutdown(&self) -> Result<()> {
    // 1. Stop accepting new events
    self.event_router.stop_accepting();
    
    // 2. Wait for in-flight tasks to complete (with timeout)
    let timeout = Duration::from_secs(30);
    match tokio::time::timeout(timeout, self.wait_for_tasks()).await {
        Ok(_) => {
            // All tasks completed cleanly
        }
        Err(_) => {
            // Timeout — tasks remain in journal as incomplete
            // They'll be recovered on next startup
            self.audit_logger.log(AuditEvent::ShutdownTimeout {
                pending_tasks: self.active_task_count(),
            });
        }
    }
    
    // 3. Flush audit log
    self.audit_logger.flush().await?;
    
    // 4. Close vault databases (ensures WAL is checkpointed)
    self.vault.close()?;
    
    // 5. Stop adapters
    self.adapter_manager.shutdown_all().await;
    
    // 6. Kill any remaining containers
    self.container_manager.kill_all().await?;
    
    // 7. Log shutdown
    eprintln!("PFAR kernel shut down cleanly");
    
    Ok(())
}
```

If the kernel is force-killed (`SIGKILL`, OOM, power loss), the task journal and pending approvals are already on disk. The next startup will recover them.

---

## 10. Configuration Additions

Add to `config.toml`:

```toml
[recovery]
# Max age of a task to attempt recovery (seconds)
# Tasks older than this are abandoned on restart
max_task_age_seconds = 600  # 10 minutes

# Graceful shutdown timeout (seconds)
# After this, in-flight tasks are left for recovery
shutdown_timeout_seconds = 30

# Journal cleanup retention
journal_retention_completed_hours = 24
journal_retention_failed_days = 7

# Adapter reconnection
adapter_max_retries = 5
adapter_retry_backoff_seconds = 5
adapter_retry_backoff_max_seconds = 60
```

---

## 11. Regression Tests

| # | Test | Validates |
|---|---|---|
| R1 | Create task, advance to Executing with 1 completed step. Kill kernel. Restart. Task resumes from step 2. | Task journal + resume |
| R2 | Create task, advance to AwaitingApproval. Kill kernel. Restart. Approval message re-sent to owner. Owner approves. Task completes. | Approval persistence + reprompt |
| R3 | Create task in Planning phase. Kill kernel. Restart. Task retries from scratch. No duplicate side effects. | Retry from scratch |
| R4 | Create task with a write step that was in-progress when kernel killed. On recovery, owner is asked whether to retry the write. | Idempotency for writes |
| R5 | Create 3 tasks: one 5 min old (recoverable), one 15 min old (abandoned). Restart. Only the young task is recovered. Old one abandoned with notification. | Max age enforcement |
| R6 | Start Telegram adapter. Kill kernel. Restart. Telegram resumes from last offset. No duplicate message processing. | Adapter offset persistence |
| R7 | Start browser container for a task. Kill kernel. Restart. Orphaned container detected and killed within 30s. | Container reconciliation |
| R8 | Send SIGTERM to kernel with 2 in-flight tasks. Both complete within 30s timeout. Clean shutdown. | Graceful shutdown |
| R9 | Send SIGTERM to kernel with a slow task. Timeout exceeded. Task remains in journal. Restart recovers it. | Shutdown timeout + recovery |
| R10 | Credential prompt pending. Kill kernel. Restart. Credential prompt re-sent. Owner provides token. Task completes. | Credential persistence |
| R11 | Kill kernel. Restart. Owner receives recovery report listing retried, resumed, abandoned tasks. | Startup notification |
| R12 | Clean restart with no pending tasks. Owner receives "No pending tasks to recover." | Clean startup path |

---

## 12. Implementation Checklist

### Database schema (add to Phase 1: Kernel Core)

- [ ] `task_journal` table in sessions.db
- [ ] `pending_approvals` table in sessions.db
- [ ] `pending_credentials` table in sessions.db
- [ ] `adapter_state` table in sessions.db
- [ ] Journal cleanup query (daily)

### Task journal writes (add to Phase 2: Pipeline)

- [ ] Journal entry created on task creation
- [ ] Updated at each phase transition (Extract→Plan→Execute→Synthesize→Complete)
- [ ] Execute progress updated after each step completion
- [ ] State set to AwaitingApproval / AwaitingCredential on suspension
- [ ] State set to Failed on error, Completed on success

### Recovery logic (add to Phase 2: Pipeline)

- [ ] `recover_tasks()` called during startup
- [ ] `determine_recovery_action()` routing by task state + age
- [ ] `RetryFromScratch` for Extract/Plan phase tasks
- [ ] `ResumeExecution` for mid-Execute tasks (with idempotency check)
- [ ] `Resynthesize` for tasks with complete tool results
- [ ] `RepromptApproval` for pending approvals
- [ ] `RepromptCredential` for pending credential prompts
- [ ] `Abandon` for tasks exceeding max age
- [ ] Owner notification with RecoveryReport

### Graceful shutdown (add to Phase 2: Pipeline)

- [ ] SIGTERM/SIGINT handler
- [ ] Wait for in-flight tasks with timeout
- [ ] Flush audit log
- [ ] Checkpoint vault WAL
- [ ] Kill containers

### Adapter state (add per-adapter in Phase 2/4)

- [ ] Telegram: persist `last_offset` after each update batch
- [ ] Telegram: resume from persisted offset on startup
- [ ] Slack: reconnect via Socket Mode on startup
- [ ] WhatsApp: detect expired session, notify owner
- [ ] Adapter startup order enforcement
- [ ] Adapter retry with exponential backoff
