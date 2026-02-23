# Flatline

The supervisor process for Wintermute. A separate binary that watches,
diagnoses, and heals — without interfering with the working agent.

Named after Dixie Flatline — the expert hacker construct in Neuromancer
who helps debug problems in the matrix. And "flatline" is literally
what a health monitor detects.

---

## The Problem Flatline Solves

Wintermute modifies itself. It writes tools, installs packages, edits
its own config, accumulates memory. Over time, things break:

- A tool the agent wrote has a bug that only surfaces on Tuesdays
- A pip package update broke an existing tool
- The agent got into a reasoning loop and burned through its daily budget
- Memory got polluted with contradictory facts
- The container won't start after a bad requirements.txt change
- A scheduled task is failing silently every night
- The agent's context is bloated with 50 dynamic tools, most unused

The agent can't reliably fix itself WHILE it's broken. A hallucinating
agent can't diagnose its own hallucinations. A crashed process can't
restart itself. A tool stuck in a loop can't break out of it.

Flatline is the separate pair of eyes.

---

## Design Principles

**D1: Separate process, separate concerns.** Flatline is a different binary
with its own PID. If Wintermute crashes, Flatline is still running. If
Flatline crashes, Wintermute is unaffected.

**D2: Read-mostly.** Flatline reads logs, reads git history, reads memory.db,
reads health files. It RARELY writes — only to apply fixes. Most of the
time it's a passive observer that occasionally speaks up.

**D3: Escalate before acting.** Flatline tells the user what's wrong and
what it wants to do BEFORE doing it. For critical fixes (Wintermute is
down), it acts first and reports after. Everything else: propose → approve → act.

**D4: Own model budget.** Flatline uses its own LLM calls (cheap model by
default). Its diagnostics don't eat Wintermute's budget. Its reasoning
is independent — different model, different context, different biases.

**D5: Can't make things worse.** Flatline's interventions are always
reversible. Git revert, not git reset --hard. Restart, not reconfigure.
Quarantine a tool, not delete it.

---

## Architecture

```
┌─ HOST ──────────────────────────────────────────────────────┐
│                                                               │
│  wintermute (main agent)         flatline (supervisor)         │
│  ├── Agent loop                  ├── Log watcher             │
│  ├── Tools + executor            ├── Health checker          │
│  ├── Memory engine               ├── Diagnostic engine       │
│  ├── Telegram adapter            ├── Fix proposer            │
│  └── writes → logs/              └── Fix applier             │
│               ↓                         ↓ reads               │
│                                                               │
│  ~/.wintermute/                                              │
│  ├── logs/*.jsonl          ← Flatline reads these              │
│  ├── scripts/.git/         ← Flatline inspects + reverts       │
│  ├── data/memory.db        ← Flatline reads (never writes)     │
│  ├── health.json           ← Wintermute writes, Flatline reads │
│  ├── flatline/                                                  │
│  │   ├── state.db          ← Flatline's own state              │
│  │   ├── diagnoses/        ← Diagnosis reports               │
│  │   └── patches/          ← Proposed + applied fixes        │
│  └── flatline.toml           ← Flatline config                   │
│                                                               │
└───────────────────────────────────────────────────────────────┘
```

Flatline and Wintermute communicate through the filesystem, not IPC.
No sockets, no shared memory, no message passing. Flatline reads what
Wintermute writes (logs, health file, git, db). Flatline writes fixes
to the filesystem (git revert, quarantine marker). Wintermute picks
up changes on its next cycle.

The one exception: Flatline can send Telegram messages to the user
(using the same bot token, different message thread or prefix).
And Flatline can send SIGTERM to Wintermute to trigger a graceful restart.

---

## What Flatline Watches

### 1. Structured Logs

Wintermute writes structured JSON logs (one per line, .jsonl):

```json
{"ts":"2026-02-19T14:30:00Z","level":"info","event":"tool_call","tool":"news_digest","duration_ms":1200,"success":true,"session":"abc123"}
{"ts":"2026-02-19T14:30:05Z","level":"error","event":"tool_call","tool":"deploy_check","duration_ms":30000,"success":false,"error":"timeout","session":"abc123"}
{"ts":"2026-02-19T14:31:00Z","level":"warn","event":"budget","tokens_used":480000,"tokens_limit":500000,"session":"abc123"}
```

Flatline tails these logs and builds a rolling window of events.

Key events Flatline cares about:
- `tool_call` with `success: false` — tool failures
- `tool_call` with high `duration_ms` — performance degradation
- `budget` approaching limits — cost anomalies
- `llm_call` with retries — model issues
- `approval` events — what the user is being asked
- `error` level anything — something unexpected

### 2. Health File

Wintermute writes ~/.wintermute/health.json every heartbeat cycle:

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
  "budget_today": { "used": 120000, "limit": 5000000 },
  "last_error": null
}
```

If this file stops updating (stale >2× heartbeat interval), Wintermute
is probably crashed or hung.

### 3. Git History (/scripts)

Flatline reads git log to understand what changed and when:

```
a1b2c3d  2026-02-19 14:00  create tool: deploy_check
f4e5d6a  2026-02-19 13:45  update config: add scheduled task
8c9d0e1  2026-02-18 09:00  create tool: news_digest
```

Correlates changes with failures: "deploy_check started failing at
14:05, and the last change to deploy_check was at 14:00."

### 4. Memory Database

Flatline reads memory.db (read-only connection) to check for:
- Memory bloat (too many pending items)
- Contradictions (conflicting facts with similar embeddings)
- Stale skills (tools referenced in memory that no longer exist)

### 5. Tool Execution Stats

Derived from logs. Flatline maintains a rolling scorecard per tool:

```
news_digest:    last 30 days: 28 success, 2 failure, avg 1.2s
deploy_check:   last 30 days: 5 success, 25 failure, avg 28s  ← problem
weather:        last 30 days: 30 success, 0 failure, avg 0.8s
```

---

## Diagnostic Engine

Flatline runs periodic health checks (every 5 minutes by default) plus
reactive checks triggered by log events.

### Check Categories

**Process health:**
- Is Wintermute running? (PID file / process check)
- Is health.json fresh? (stale = hung or crashed)
- Is the container running? (Docker ps equivalent)
- Is memory usage reasonable? (host + container)

**Tool health:**
- Failure rate per tool over rolling window
- Timeout rate per tool
- Tools that haven't been used in 30+ days (candidates for cleanup)
- Tools that were recently changed and started failing

**Budget health:**
- Burn rate: at current pace, will daily budget be exhausted early?
- Cost per session trending up? (possible reasoning loops)
- Unusual spike in tool calls per turn?

**Memory health:**
- Database size growth rate
- Pending items accumulating? (observer not promoting or user not reviewing)
- FTS index health

**Scheduled task health:**
- Did last scheduled execution succeed?
- Is a scheduled task consistently failing?
- Is notify=true but user never receives output?

### Diagnosis Flow

```
Event/Timer
    │
    ▼
Gather evidence (logs, health.json, git log, memory stats)
    │
    ▼
Pattern matching (rules, not LLM — fast, cheap)
    │
    ├── Known pattern found → Propose specific fix
    │
    └── Unknown pattern → LLM diagnosis
                              │
                              ▼
                         Diagnosis report
                              │
                              ▼
                         Propose fix (or just report)
```

Rules first, LLM second. Most issues are recognizable patterns
(tool failing after recent change, container down, budget spike).
The LLM is reserved for novel problems that rules don't catch.

---

## Known Patterns + Automatic Fixes

### Pattern: Tool failing after recent change

**Detection:** tool X failure rate > 50% in last hour AND git log shows
change to X within last 2 hours.

**Fix:** Quarantine tool (rename X.json → X.json.quarantined), git revert
the change, notify user.

**Severity:** Medium. Auto-fix if enabled, otherwise propose.

### Pattern: Wintermute process down

**Detection:** health.json stale > 3× heartbeat interval AND process
not running.

**Fix:** Restart wintermute process. If it crashes again within 5 minutes,
report to user and stop retrying.

**Severity:** Critical. Always auto-fix (restart). Escalate on repeated failure.

### Pattern: Container won't start

**Detection:** health.json shows container_healthy: false repeatedly.
Or Wintermute logs show Docker errors.

**Fix:** Reset sandbox (wintermute reset equivalent). If that fails,
try removing last requirements.txt change (git revert) and reset again.

**Severity:** High. Auto-fix first attempt, escalate on failure.

### Pattern: Budget exhaustion loop

**Detection:** >80% of daily budget used in <25% of the day. OR single
session using >3× average tokens.

**Fix:** No auto-fix. Alert user immediately. Include breakdown of what
consumed the budget (which sessions, which tools).

**Severity:** Medium. Report only.

### Pattern: Scheduled task consistently failing

**Detection:** Scheduled task X has failed 3+ consecutive executions.

**Fix:** Disable the task (set enabled=false in agent.toml, git commit).
Notify user with failure details.

**Severity:** Medium. Auto-fix (disable), notify.

### Pattern: Memory bloat

**Detection:** >100 pending memories not promoted or rejected in 7+ days.

**Fix:** No auto-fix. Suggest to user: "You have 147 pending memories.
Review with /memory pending or switch to auto-promote."

**Severity:** Low. Report only.

### Pattern: Dynamic tool sprawl

**Detection:** >40 dynamic tools. Or >15 tools unused in 30+ days.

**Fix:** No auto-fix. Report to user with list of unused tools and
suggestion to archive.

**Severity:** Low. Report only.

### Pattern: Disk space pressure

**Detection:** ~/.wintermute/ exceeds configured threshold (default 5GB).
Or host disk <10% free.

**Fix:** Suggest cleanup: old logs, Docker image prune, workspace temp
files. Auto-prune logs older than retention period.

**Severity:** Medium. Auto-prune logs, suggest rest.

---

## Fix Lifecycle

Every fix follows the same lifecycle, regardless of severity.

```
DETECTED → DIAGNOSED → PROPOSED → [APPROVED] → APPLIED → VERIFIED

For auto-fix:
DETECTED → DIAGNOSED → APPLIED → VERIFIED → REPORTED

For manual:
DETECTED → DIAGNOSED → PROPOSED → user approves → APPLIED → VERIFIED
```

### Fix Record

Every fix is persisted in flatline/patches/:

```json
{
  "id": "fix-20260219-001",
  "detected_at": "2026-02-19T14:05:00Z",
  "pattern": "tool_failing_after_change",
  "evidence": {
    "tool": "deploy_check",
    "failure_rate": 0.83,
    "last_change": "a1b2c3d",
    "change_time": "2026-02-19T14:00:00Z"
  },
  "diagnosis": "deploy_check started failing immediately after last commit. The change likely introduced a bug.",
  "action": "quarantine_and_revert",
  "details": {
    "quarantined": "deploy_check.json → deploy_check.json.quarantined",
    "reverted_commit": "a1b2c3d",
    "new_commit": "e5f6g7h"
  },
  "applied_at": "2026-02-19T14:06:00Z",
  "verified": true,
  "verified_at": "2026-02-19T14:11:00Z",
  "user_notified": true
}
```

### Verification

After applying a fix, Flatline checks if it worked:

- Tool quarantine: is the tool no longer failing? (it shouldn't be called)
- Git revert: does the previous version pass a test execution?
- Process restart: is health.json fresh again?
- Scheduled task disable: no more failure events for that task?

If verification fails, escalate to user.

---

## LLM Usage

Flatline uses LLM calls sparingly. Most diagnosis is rule-based.

LLM is called for:
1. **Novel problems** — failure pattern that doesn't match known rules
2. **Root cause analysis** — when a rule matches but the fix didn't work
3. **Writing diagnosis reports** — human-readable explanation of what went wrong

Flatline uses its own model (configured separately from Wintermute):

```toml
# flatline.toml
[model]
default = "ollama/qwen3:8b"       # cheap, local
# fallback = "anthropic/claude-haiku-4-5-20251001"

[budget]
max_tokens_per_day = 100000       # Flatline's own budget, separate from Wintermute
```

### LLM Diagnosis Prompt

When rules don't match, Flatline sends:

```
You are a system diagnostician. Analyze these events and identify
the likely root cause.

## Recent Events
{last 50 log entries, filtered to errors/warnings}

## Recent Changes
{git log --oneline -10 from /scripts}

## Current Health
{health.json contents}

## Tool Stats
{failure rates for tools involved}

Respond with:
1. Root cause (one sentence)
2. Confidence (high/medium/low)
3. Recommended action (one of: revert_commit, quarantine_tool,
   restart_process, reset_sandbox, report_only)
4. Details (what specifically to revert/quarantine/report)
```

Response parsed as structured output. If confidence is low,
Flatline reports to user instead of acting.

---

## Communication with User

Flatline sends Telegram messages with a [🩺 Flatline] prefix so the user
can distinguish them from Wintermute's messages.

### Message Types

**Status report** (periodic, configurable):
```
🩺 Flatline — Daily Health Report

✅ Wintermute: running (uptime 3d 14h)
✅ Container: healthy
✅ Budget: 12% used today
⚠️  deploy_check: 83% failure rate (last 24h)
✅ 22 tools active, all others healthy
✅ Memory: 1,247 facts, 89 procedures
📦 Backup: last successful 03:00 today
```

**Alert** (immediate, triggered by issue):
```
🩺 Flatline — Alert

Tool `deploy_check` has been failing since 14:00.
It was last modified at 14:00 (commit a1b2c3d).

I've quarantined the tool and reverted to the previous version.
The previous version is now active and passing.

[🔍 View Diff] [↩️ Undo My Fix] [📋 Full Report]
```

**Proposal** (needs approval):
```
🩺 Flatline — Proposal

Your scheduled task `news_digest` has failed 5 times in a row.
Last error: "ModuleNotFoundError: feedparser"

I'd like to:
1. Disable the scheduled task
2. Add feedparser to requirements.txt
3. Reset the sandbox to install it
4. Re-enable the task

[✅ Approve All] [🔧 Let Me Handle It]
```

### Report Frequency

Configurable:
```toml
[reports]
daily_health = "08:00"           # daily summary, or false to disable
alert_cooldown_mins = 30         # don't spam about same issue
```

---

## Flatline's Own State

Flatline maintains its own SQLite database (flatline/state.db):

```sql
-- Rolling tool health stats
CREATE TABLE tool_stats (
    tool_name TEXT NOT NULL,
    window_start TEXT NOT NULL,    -- hourly buckets
    success_count INTEGER DEFAULT 0,
    failure_count INTEGER DEFAULT 0,
    avg_duration_ms INTEGER,
    PRIMARY KEY (tool_name, window_start)
);

-- Fix history
CREATE TABLE fixes (
    id TEXT PRIMARY KEY,
    detected_at TEXT NOT NULL,
    pattern TEXT,
    diagnosis TEXT,
    action TEXT,
    applied_at TEXT,
    verified BOOLEAN,
    user_notified BOOLEAN
);

-- Known issues (suppressed alerts)
CREATE TABLE suppressions (
    pattern TEXT PRIMARY KEY,
    suppressed_until TEXT,
    reason TEXT
);
```

---

## Interactions with Wintermute

Flatline never calls Wintermute's tools or APIs. All interaction is
through the filesystem and signals.

| Flatline action | Mechanism |
|---------------|-----------|
| Read logs | Tail ~/.wintermute/logs/*.jsonl |
| Read health | Read ~/.wintermute/health.json |
| Read git history | git -C ~/.wintermute/scripts log |
| Revert a tool | git -C ~/.wintermute/scripts revert {commit} |
| Quarantine a tool | mv {tool}.json {tool}.json.quarantined |
| Restart Wintermute | SIGTERM → wait → SIGKILL if needed → start |
| Reset sandbox | wintermute reset (CLI command) |
| Edit agent.toml | Direct file write (e.g., disable scheduled task) |
| Notify user | Telegram bot API (same token, [🩺 Flatline] prefix) |

Wintermute picks up changes:
- /scripts/*.json changes → hot-reload detects missing/new tools
- agent.toml changes → picked up on next heartbeat cycle
- Container reset → Wintermute reconnects to new container
- Process restart → Wintermute starts fresh, loads latest state

---

## What Flatline Cannot Do

- **Cannot modify config.toml** — security policy is human-only
- **Cannot delete tools** — only quarantine (reversible) or revert
- **Cannot modify memory.db** — read-only access
- **Cannot change budget limits** — that's config.toml
- **Cannot approve things on behalf of the user** — only Wintermute's
  approval flow handles that
- **Cannot modify Wintermute's binary** — only operates on data/config
- **Cannot install packages** — that's Wintermute's job
- **Cannot access the sandbox** — no Docker interaction

Flatline is deliberately less powerful than Wintermute. It can roll back,
restart, quarantine, disable, and report. It cannot create, install,
configure, or grant permissions.

---

## Binary & Deployment

Flatline is a separate Rust binary in the same repo:

```
wintermute/
├── src/                    # Main agent
├── flatline/
│   ├── src/
│   │   ├── main.rs         # CLI + daemon loop
│   │   ├── config.rs       # flatline.toml loading
│   │   ├── watcher.rs      # Log tailing + health file monitoring
│   │   ├── stats.rs        # Rolling tool/budget/memory statistics
│   │   ├── patterns.rs     # Known failure pattern matching
│   │   ├── diagnosis.rs    # LLM-based diagnosis (novel problems)
│   │   ├── fixer.rs        # Fix proposal + application + verification
│   │   ├── reporter.rs     # Telegram notifications + daily reports
│   │   └── db.rs           # state.db management
│   └── Cargo.toml
├── Cargo.toml              # workspace
└── flatline.toml.example
```

Cargo workspace. Both binaries built from same repo:
```bash
cargo build --release -p wintermute
cargo build --release -p flatline
```

Or as a single install:
```bash
wintermute start            # starts the agent
wintermute flatline start     # starts the supervisor (could be subcommand)
```

### Process Management

Both processes managed by systemd (Linux) or launchd (macOS),
or just run in separate terminal sessions / tmux panes.

```ini
# /etc/systemd/system/wintermute.service
[Unit]
Description=Wintermute AI Agent
After=network.target docker.service

[Service]
ExecStart=/usr/local/bin/wintermute start
Restart=on-failure
RestartSec=10
User=wintermute

[Install]
WantedBy=multi-user.target
```

```ini
# /etc/systemd/system/wintermute-flatline.service
[Unit]
Description=Wintermute Flatline (Supervisor)
After=wintermute.service
BindsTo=wintermute.service

[Service]
ExecStart=/usr/local/bin/flatline start
Restart=always
RestartSec=5
User=wintermute

[Install]
WantedBy=multi-user.target
```

Flatline starts after Wintermute. If Wintermute's service is stopped
intentionally, Flatline stops too (BindsTo). But if Wintermute crashes,
Flatline stays running and handles the restart.

---

## Configuration

```toml
# flatline.toml

[model]
default = "ollama/qwen3:8b"
# fallback = "anthropic/claude-haiku-4-5-20251001"

[budget]
max_tokens_per_day = 100_000

[checks]
interval_secs = 300                # periodic health check every 5 min
health_stale_threshold_secs = 180  # health.json stale after 3× heartbeat

[thresholds]
tool_failure_rate = 0.5            # alert if >50% failure in window
tool_failure_window_hours = 1      # rolling window for failure rate
budget_burn_rate_alert = 0.8       # alert if >80% used in <25% of day
memory_pending_alert = 100         # alert if >100 pending memories
unused_tool_days = 30              # suggest cleanup after 30 days unused
max_tool_count_warning = 40        # warn about tool sprawl
disk_warning_gb = 5                # warn when ~/.wintermute > 5GB

[auto_fix]
enabled = true
restart_on_crash = true            # auto-restart Wintermute
quarantine_failing_tools = true    # auto-quarantine after threshold
disable_failing_tasks = true       # auto-disable after 3 consecutive failures
revert_recent_changes = true       # auto-revert if correlated with failure
max_auto_restarts_per_hour = 3     # stop retrying after N restarts

[reports]
daily_health = "08:00"             # daily health summary
alert_cooldown_mins = 30           # don't repeat same alert within window
telegram_prefix = "🩺 Flatline"

[telegram]
bot_token_env = "WINTERMUTE_TELEGRAM_TOKEN"  # same bot
notify_users = [123456789]                    # same user(s)
```

---

## Implementation Plan

Flatline is a v1.1 deliverable. It requires Wintermute v1 to be stable
and producing structured logs.

### Prerequisites from Wintermute v1

These are already in the Wintermute design but called out here as
Flatline dependencies:

- [x] Structured JSON logging (event types, tool names, durations, errors)
- [x] health.json written by heartbeat
- [x] Git-versioned /scripts
- [x] agent.toml separate from config.toml

### Flatline Build Order

**Week 1: Foundation**
- Scaffold: CLI, config loading, process management
- Log watcher: tail .jsonl files, parse events
- Health file monitor: detect stale health.json
- Stats engine: rolling window per-tool success/failure rates

**Week 2: Patterns + Fixes**
- Pattern matcher: all known patterns from the design
- Fix proposer: generate fix records from pattern matches
- Fix applier: git revert, quarantine, restart, disable task
- Verification: check if fix worked after application

**Week 3: Reporting + LLM**
- Telegram reporter: alerts, proposals, daily health
- LLM diagnosis: novel problem analysis via cheap model
- Approval flow: propose → user approves → apply
- State database: tool stats, fix history, suppressions

**Week 4: Hardening**
- Edge cases: Flatline crashes during fix application
- Idempotency: same fix not applied twice
- Cooldowns: don't spam alerts, don't restart in a loop
- Testing: simulate failure scenarios

---

## Future Extensions (v1.2+)

**Predictive health:** Detect trends before they become failures.
Tool X's latency has been increasing 10% per week — investigate.

**Automated testing:** After reverting a tool, run the tool with
a sample input to verify it works before declaring it fixed.

**Multi-agent coordination:** If multiple Wintermute instances exist
(different users/machines), Flatline aggregates health across fleet.

**Self-improvement suggestions:** "Tool X is called 50 times/day
but takes 30 seconds each time. You could optimize it by caching
the API response."

**Anomaly detection:** Learn normal behavior patterns, alert on
deviations without predefined rules. Requires more historical data.
