# PFAR Feature Spec: Persistence & Recovery

> **Feature**: Runtime State Persistence and Restart Recovery
> **Status**: Implementation-ready
> **Priority**: Phase 1

---

## 1. What Already Persists

The three vault databases are SQLCipher files on disk. They survive restarts with no additional work:

- **secrets.db** — API keys, OAuth tokens, adapter credentials
- **sessions.db** — per-principal conversation history and session working memory
- **memory.db** — long-term consolidated knowledge

On startup, the kernel reads the master key from OS keychain, unlocks the databases, and all user context is intact. The Planner sees the same working memory and conversation history as before the restart.

---

## 2. What Needs to Be Added

Only one piece of runtime state matters across restarts: the Telegram update offset.

### Adapter State Table

```sql
-- In sessions.db
CREATE TABLE adapter_state (
    adapter     TEXT PRIMARY KEY,
    state_json  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
```

The Telegram adapter writes `{"last_offset": 123456789}` after each processed update batch. On restart, it resumes from `last_offset + 1` instead of reprocessing old messages.

No other adapter needs persistent state — Slack reconnects statelessly via Socket Mode, WhatsApp session credentials are already in secrets.db, webhooks are stateless.

---

## 3. Startup Sequence

```rust
pub async fn startup(&self) -> Result<()> {
    // 1. Load config
    let config = Config::load_from_disk()?;

    // 2. Unlock vault (master key from OS keychain)
    self.vault.unlock(&config.vault)?;

    // 3. Start audit logger
    self.audit_logger.start()?;

    // 4. Kill orphaned browser/script containers from before crash
    let orphans = self.container_manager.kill_all_managed().await?;

    // 5. Start core services
    self.policy_engine.load(&config.data_flow)?;
    self.tool_registry.load(&config)?;
    self.template_registry.load_from_dir(&config.templates_dir)?;
    self.inference_proxy.start(&config.llm).await?;
    self.scheduler.start(&config)?;

    // 6. Start adapters (primary channel first)
    self.start_adapters(&config).await;

    // 7. Start container reconciliation loop
    self.container_manager.start_reconciliation_loop();

    // 8. Notify owner
    let msg = if orphans > 0 {
        format!(
            "System restarted. Cleaned up {} orphaned containers. \
             If you were waiting on something, just ask again.",
            orphans
        )
    } else {
        "System restarted. If you were waiting on something, just ask again.".into()
    };
    self.notify_owner(&msg).await?;

    Ok(())
}
```

---

## 4. Graceful Shutdown

On `SIGTERM` / `SIGINT`:

```rust
pub async fn graceful_shutdown(&self) -> Result<()> {
    // 1. Stop accepting new events
    self.event_router.stop_accepting();

    // 2. Wait briefly for in-flight tasks (they're 3-5s max)
    tokio::time::sleep(Duration::from_secs(5)).await;

    // 3. Flush audit log
    self.audit_logger.flush().await?;

    // 4. Close vault databases (checkpoint WAL)
    self.vault.close()?;

    // 5. Kill containers
    self.container_manager.kill_all_managed().await?;

    Ok(())
}
```

If the kernel is force-killed, in-flight tasks are lost. The user retries. The vault data is safe (SQLCipher handles incomplete writes).

---

## 5. In-Flight Task Policy

Tasks take 3-5 seconds. If the kernel crashes mid-task, the task is lost. No journaling, no resume, no replay.

The owner's startup notification ("If you were waiting on something, just ask again") covers this. For a personal assistant, re-asking is simpler and more reliable than attempting to recover a half-finished plan.

---

## 6. Adapter Reconnection

Adapters start in priority order:

```
1. CLI         — instant, always works
2. Telegram    — resume polling from saved offset
3. Webhooks    — restart HTTP listener
4. Slack       — reconnect WebSocket
5. WhatsApp    — reload Baileys session, notify owner if expired
```

If an adapter fails to connect, the kernel logs it and continues. It retries with exponential backoff (5s, 10s, 20s, 40s, 60s max — 5 attempts). If all retries fail, the owner is notified via any healthy adapter.

WhatsApp-specific: if the Baileys session expired during downtime, notify the owner to re-pair.

---

## 7. Implementation Checklist

- [x] `adapter_state` table in sessions.db
- [x] Telegram adapter: write `last_offset` after each update batch
- [x] Telegram adapter: read `last_offset` on startup, resume from there
- [x] Startup sequence: unlock vault → start services → start adapters → notify owner
- [x] SIGTERM/SIGINT handler: wait 5s → flush → notify owner
- [x] Session persistence: conversation_turns + working_memory tables survive restarts
- [ ] Container manager: `kill_all_managed()` for startup cleanup
- [ ] Adapter retry with exponential backoff on connection failure
- [ ] WhatsApp: detect expired session, notify owner
