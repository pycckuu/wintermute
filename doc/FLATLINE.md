# Flatline

The supervisor process for Wintermute. A separate binary that watches,
diagnoses, and heals — without interfering with the working agent.

Named after Dixie Flatline — the expert hacker construct in Neuromancer
who helps debug problems in the matrix. And "flatline" is literally
what a health monitor detects.

---

## The Problem Flatline Solves

Wintermute modifies itself. It writes tools, installs packages, edits
its own config, evolves its personality, accumulates memory. Over time,
things break:

- A tool the agent wrote has a bug that only surfaces on Tuesdays
- A pip package update broke an existing tool
- A bad setup.sh change broke the container on reset
- The agent got into a reasoning loop and burned through its daily budget
- Memory got polluted with contradictory facts
- The container won't start after a bad apt-get install
- A scheduled task is failing silently every night
- The agent's context is bloated with 50 dynamic tools, most unused
- The agent modified its soul in a way that degraded its helpfulness

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
reads health files, reads tool _meta. It RARELY writes — only to apply
fixes. Most of the time it's a passive observer that occasionally speaks up.

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
│  └── writes → logs/              ├── Fix applier             │
│               ↓                  ├── Updater                 │
│                                  └── Reporter                 │
│                                         ↓ reads + writes      │
│  ~/.wintermute/                                              │
│  ├── logs/*.jsonl          ← Flatline reads these              │
│  ├── scripts/                                                  │
│  │   ├── .git/             ← Flatline inspects + reverts       │
│  │   ├── setup.sh          ← Flatline reverts on bad install   │
│  │   └── *.json (_meta)    ← Flatline reads tool health        │
│  ├── data/memory.db        ← Flatline reads (never writes)     │
│  ├── AGENTS.md             ← Flatline reads (context for diag) │
│  ├── health.json           ← Wintermute writes, Flatline reads │
│  ├── flatline/                                                  │
│  │   ├── state.db          ← Flatline's own state              │
│  │   ├── diagnoses/        ← Diagnosis reports               │
│  │   ├── patches/          ← Proposed + applied fixes        │
│  │   └── updates/          ← Downloaded binaries + rollback  │
│  │       ├── pending/      ← Downloaded, not yet applied     │
│  │       ├── wintermute.prev ← Rollback binary               │
│  │       ├── flatline.prev   ← Rollback binary               │
│  │       └── last_update.json ← Update log                    │
│  └── flatline.toml           ← Flatline config                   │
│                                                               │
│  GitHub Releases API ← Flatline checks daily                   │
│  Docker Registry     ← Flatline pulls images                   │
│                                                               │
└───────────────────────────────────────────────────────────────┘
```

Flatline and Wintermute communicate through the filesystem, not IPC.
No sockets, no shared memory, no message passing. Flatline reads what
Wintermute writes (logs, health file, git, db, tool schemas). Flatline
writes fixes to the filesystem (git revert, quarantine marker). Wintermute
picks up changes on its next cycle.

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
- `escalation` — agent needed help from oracle
- `approval` events — what the user is being asked
- `soul_modified` — agent changed its personality
- `no_reply` — agent chose silence (group chat or proactive)
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
  "dynamic_tools_count": 18,
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
b2c3d4e  2026-02-17 22:00  setup: add ffmpeg
d5e6f7g  2026-02-17 21:30  evolve: more concise communication
```

Correlates changes with failures: "deploy_check started failing at
14:05, and the last change to deploy_check was at 14:00."

Also tracks setup.sh and soul changes — these can cause subtle
failures (broken container on reset, degraded agent behavior).

### 4. Memory Database

Flatline reads memory.db (read-only connection) to check for:
- Memory bloat (too many pending items)
- Contradictions (conflicting facts with similar embeddings)
- Stale skills (tools referenced in memory that no longer exist)

### 5. Tool Health Metadata

Dynamic tool schemas include a `_meta` field maintained by Wintermute's
tool registry:

```json
{
  "name": "deploy_check",
  "_meta": {
    "created_at": "2026-02-19T14:00:00Z",
    "last_used": "2026-02-25T08:00:00Z",
    "invocations": 30,
    "success_rate": 0.17,
    "avg_duration_ms": 28000,
    "last_error": "timeout at 120s",
    "version": 3
  }
}
```

Flatline reads `_meta` directly from `/scripts/*.json` for:
- Pre-aggregated health stats (no log parsing needed for basic view)
- Tool age and usage patterns
- Success rate trends (compare current _meta with previous git versions)

Flatline still uses log-based stats for real-time alerting (detecting
failures as they happen), but `_meta` provides the persistent,
pre-aggregated view for reports and trend analysis.

### 6. Tool Execution Stats (from logs)

Derived from logs for real-time awareness. Flatline maintains a rolling
scorecard per tool:

```
news_digest:    last 30 days: 28 success, 2 failure, avg 1.2s
deploy_check:   last 30 days: 5 success, 25 failure, avg 28s  ← problem
weather:        last 30 days: 30 success, 0 failure, avg 0.8s
```

This complements `_meta` — logs give real-time events, `_meta` gives
the aggregated picture.

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
- Success rate declining over time (compare _meta across git versions)
- Latency increasing over time

**Budget health:**
- Burn rate: at current pace, will daily budget be exhausted early?
- Cost per session trending up? (possible reasoning loops)
- Unusual spike in tool calls per turn?
- Escalation frequency (too many oracle calls = possible model mismatch)

**Memory health:**
- Database size growth rate
- Pending items accumulating? (observer not promoting or user not reviewing)
- FTS index health

**Scheduled task health:**
- Did last scheduled execution succeed?
- Is a scheduled task consistently failing?
- Is notify=true but user never receives output?

**Sandbox health:**
- Does setup.sh execute cleanly?
- Are there orphaned packages (in container but not in setup.sh)?
- Is the container using excessive disk?

**Personality health:**
- Was the soul recently modified? (log soul_modified events)
- Has agent behavior degraded after soul change? (more errors, more
  escalations, user complaints)

### Diagnosis Flow

```
Event/Timer
    │
    ▼
Gather evidence (logs, health.json, git log, _meta, memory stats)
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

**Fix:** Reset sandbox (`wintermute reset` equivalent — recreates container,
runs setup.sh). If setup.sh itself is broken, try reverting the last
setup.sh change (`git revert` the relevant commit) and reset again.

**Severity:** High. Auto-fix first attempt, escalate on failure.

### Pattern: Bad setup.sh

**Detection:** Container creation/reset fails AND git log shows recent
change to setup.sh.

**Fix:** Git revert the setup.sh change, reset sandbox again. Notify user
with the offending change.

**Severity:** High. Auto-fix (revert + reset), notify.

### Pattern: Budget exhaustion loop

**Detection:** >80% of daily budget used in <25% of the day. OR single
session using >3× average tokens.

**Fix:** No auto-fix. Alert user immediately. Include breakdown of what
consumed the budget (which sessions, which tools, escalation calls).

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

**Detection:** >40 dynamic tools. Or >15 tools unused in 30+ days
(read directly from _meta.last_used).

**Fix:** No auto-fix. Report to user with list of unused tools and
suggestion to archive.

**Severity:** Low. Report only.

### Pattern: Disk space pressure

**Detection:** ~/.wintermute/ exceeds configured threshold (default 5GB).
Or host disk <10% free.

**Fix:** Suggest cleanup: old logs, Docker image prune, workspace temp
files. Auto-prune logs older than retention period.

**Severity:** Medium. Auto-prune logs, suggest rest.

### Pattern: Soul regression

**Detection:** soul_modified event in logs AND subsequent increase in
error rate, escalation frequency, or user-initiated /revert commands.

**Fix:** No auto-fix. Report to user: "Agent personality was modified
at {time}. Since then, error rate increased from X% to Y%. Consider
reviewing the change or running /revert."

**Severity:** Low. Report only.

### Pattern: Excessive escalation

**Detection:** >10 escalation calls per day, or >3 per session.

**Fix:** No auto-fix. Report to user: "Agent is escalating to the
oracle model frequently. This suggests the default model may be
underpowered for the current workload, or the agent is stuck in
a pattern. Consider upgrading the default model or reviewing
recent tasks."

**Severity:** Low. Report only.

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
    "failure_rate_source": "_meta",
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
- Setup.sh revert: does container reset succeed now?

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
default = "anthropic/claude-opus-4-6"
# fallback = "ollama/qwen3:8b"

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
{failure rates for tools involved, from _meta + logs}

## Recent Soul Changes
{if any soul_modified events in last 48h, include the diff}

## AGENTS.md
{current content — may contain relevant known issues}

Respond with:
1. Root cause (one sentence)
2. Confidence (high/medium/low)
3. Recommended action (one of: revert_commit, quarantine_tool,
   restart_process, reset_sandbox, revert_setup, report_only)
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
📉 news_digest: latency up 40% this week (1.2s → 1.7s)
✅ 22 tools active, all others healthy
📊 3 tools unused >30 days: old_scraper, test_tool, csv_parser
✅ Memory: 1,247 facts, 89 procedures
📦 Backup: last successful 03:00 today
🧠 Soul: unchanged since Feb 15
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
2. Add feedparser to setup.sh
3. Reset the sandbox
4. Re-enable the task

[✅ Approve All] [🔧 Let Me Handle It]
```

**Setup.sh alert:**
```
🩺 Flatline — Alert

Container reset failed. setup.sh exited with code 1.
Last change to setup.sh was 2 hours ago (commit b2c3d4e):
  + apt-get install -y libfoo-dev

I've reverted setup.sh and reset the sandbox.
The previous version is working.

[🔍 View Diff] [↩️ Undo My Fix]
```

### Report Frequency

Configurable:
```toml
[reports]
daily_health = "08:00"           # daily summary, or false to disable
alert_cooldown_mins = 30         # don't spam about same issue
```

### Daily Report Enhancements

The daily health report includes tool ecosystem hints:
- Tools with declining success rates (compare _meta over git history)
- Tools not used in >30 days (from _meta.last_used)
- Tools with increasing latency trends
- Upcoming monthly tool_review task status
- Recent soul changes and any correlated behavior shifts

This keeps the user aware of gradual degradation between monthly
tool reviews.

---

## Flatline's Own State

Flatline maintains its own SQLite database (flatline/state.db):

```sql
-- Rolling tool health stats (from logs, real-time)
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

-- Update history
CREATE TABLE updates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    checked_at TEXT NOT NULL,
    from_version TEXT NOT NULL,
    to_version TEXT NOT NULL,
    status TEXT NOT NULL,         -- pending | downloading | applying | healthy | rolled_back | failed | skipped | pinned
    started_at TEXT,
    completed_at TEXT,
    rollback_reason TEXT,         -- NULL if successful
    migration_log TEXT            -- stdout/stderr from migration script
);

-- Soul change tracking
CREATE TABLE soul_changes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    detected_at TEXT NOT NULL,
    commit_hash TEXT NOT NULL,
    diff_summary TEXT,
    error_rate_before REAL,       -- 24h window before change
    error_rate_after REAL,        -- populated after 24h
    escalation_rate_before REAL,
    escalation_rate_after REAL,
    flagged BOOLEAN DEFAULT FALSE -- true if regression detected
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
| Read tool health | Parse _meta from /scripts/*.json |
| Read git history | git -C ~/.wintermute/scripts log |
| Read lessons | Read ~/.wintermute/AGENTS.md |
| Revert a tool | git -C ~/.wintermute/scripts revert {commit} |
| Revert setup.sh | git -C ~/.wintermute/scripts revert {commit} |
| Revert soul change | git -C ~/.wintermute/scripts revert {commit} |
| Quarantine a tool | mv {tool}.json {tool}.json.quarantined |
| Restart Wintermute | SIGTERM → wait → SIGKILL if needed → start |
| Reset sandbox | wintermute reset (CLI command) |
| Edit agent.toml | Direct file write (e.g., disable scheduled task) |
| Update binaries | Download from GitHub, swap, restart |
| Pull Docker images | docker pull (sandbox + browser sidecar) |
| Rollback update | Restore .prev binaries, retag old images |
| Notify user | Telegram bot API (same token, [🩺 Flatline] prefix) |

Wintermute picks up changes:
- /scripts/*.json changes → hot-reload detects missing/new tools
- agent.toml changes → picked up on next heartbeat cycle
- setup.sh changes → applied on next container reset
- Container reset → Wintermute reconnects to new container
- Process restart → Wintermute starts fresh, loads latest state

---

## Auto-Update

Flatline checks for new releases daily and manages the full update
lifecycle: download, verify, swap, restart, health-check, rollback if
broken. The user stays informed and in control.

### What Gets Updated

Four artifacts, in this order:

```
1. Sandbox image       docker pull (wintermute-sandbox)
2. Browser sidecar     docker pull (wintermute-browser)
3. Wintermute binary   download + swap + restart
4. Flatline binary     download + swap + exit (systemd restarts)
```

Order matters: images first (can be pulled while Wintermute is still
running), then Wintermute (requires restart), then Flatline last (after
Wintermute is confirmed healthy with the new version).

### Update Source

GitHub Releases. Each release contains:
- Platform binaries: `wintermute-{version}-{target}.tar.gz`
  (e.g. `wintermute-0.4.0-x86_64-unknown-linux-gnu.tar.gz`)
- Checksum file: `checksums-sha256.txt`
- Docker image tags matching the release version
- Changelog in release body

```rust
// Compiled into both binaries at build time
const VERSION: &str = env!("CARGO_PKG_VERSION");
const TARGET: &str = env!("TARGET"); // set by build.rs
```

Version check:
```
GET https://api.github.com/repos/{owner}/wintermute/releases/latest
→ tag_name: "v0.4.0"
→ compare with current VERSION using semver
→ if newer: proceed
```

### Channels

```toml
[update]
channel = "stable"   # stable | nightly
```

**stable** (default): Tagged releases only. Most users. Tested.
**nightly**: Latest commit on main. Builds published to
`ghcr.io/{owner}/wintermute:nightly` and as GitHub release
marked "pre-release". For development/testing.

### Update Flow

```
Daily timer (default 04:00, configurable)
    │
    ▼
Check GitHub Releases API
    │
    ├── No new version → done
    │
    └── New version found
            │
            ▼
        Download binary + checksum (to ~/.wintermute/flatline/updates/)
            │
            ▼
        Verify SHA256 checksum
            │
            ├── Mismatch → alert user, abort
            │
            └── Checksum OK
                    │
                    ▼
                Notify user via Telegram:
                "🩺 Flatline: Update available v0.3.2 → v0.4.0
                 Changes: [summary from changelog]
                 Reply /update to install, /skip to defer"
                    │
                    ├── auto_apply = true → skip approval, proceed
                    │
                    └── User replies /update (or auto_apply)
                            │
                            ▼
                        Wait for idle window
                        (no active Wintermute sessions)
                            │
                            ▼
                        ┌── UPDATE SEQUENCE ──────────────┐
                        │                                  │
                        │ 1. Pull Docker images            │
                        │    docker pull sandbox:v0.4.0    │
                        │    docker pull browser:v0.4.0    │
                        │                                  │
                        │ 2. Stop Wintermute (SIGTERM)     │
                        │    Wait for graceful shutdown     │
                        │                                  │
                        │ 3. Backup current binary         │
                        │    cp wintermute wintermute.prev │
                        │    cp flatline flatline.prev     │
                        │                                  │
                        │ 4. Replace wintermute binary     │
                        │    mv wintermute.new wintermute  │
                        │    chmod +x wintermute           │
                        │                                  │
                        │ 5. Recreate sandbox container    │
                        │    (new image, runs setup.sh)    │
                        │                                  │
                        │ 6. Start Wintermute              │
                        │                                  │
                        │ 7. Health watch (5 min)          │
                        │    Monitor health.json           │
                        │    Check process alive           │
                        │    Check container healthy       │
                        │                                  │
                        │ 8a. HEALTHY:                     │
                        │     Replace flatline binary      │
                        │     Exit (systemd restarts new)  │
                        │     Notify: "✅ Updated to 0.4.0"│
                        │                                  │
                        │ 8b. UNHEALTHY:                   │
                        │     → Rollback (see below)       │
                        │                                  │
                        └──────────────────────────────────┘
```

### Idle Window Detection

Updates should not interrupt active work. Flatline waits for an idle
window before applying:

```rust
fn is_idle(health: &HealthFile) -> bool {
    health.active_sessions == 0
        && health.last_heartbeat.elapsed() < Duration::from_secs(60)
        // Agent is alive but not doing anything
}
```

If no idle window within the configured patience period (default 6
hours), Flatline notifies the user:
"Update pending but Wintermute has been busy. Reply /update-now to
force, or I'll try again tomorrow."

### Rollback

If Wintermute fails health checks within 5 minutes of an update:

```
Health check fails
    │
    ▼
Stop broken Wintermute (SIGKILL if needed)
    │
    ▼
Restore previous binary:  mv wintermute.prev wintermute
    │
    ▼
Restore previous image:   retag old image
    │
    ▼
Recreate container with old image (runs setup.sh)
    │
    ▼
Start Wintermute (old version)
    │
    ▼
Verify health (old version works?)
    │
    ├── Yes → Notify: "⚠️ Update to v0.4.0 failed, rolled back to v0.3.2.
    │          Error: [health check details]. Will retry on next release."
    │
    └── No → CRITICAL: Both versions broken
             Notify: "🚨 Update rollback also failed. Manual intervention needed."
             Do NOT retry. Leave in failed state for user to debug.
```

Rollback artifacts stored in `~/.wintermute/flatline/updates/`:
```
updates/
├── wintermute.prev          # previous binary
├── flatline.prev            # previous binary
├── last_update.json         # update log with timestamps + result
└── pending/                 # downloaded but not yet applied
    ├── wintermute-0.4.0     # new binary
    ├── flatline-0.4.0       # new binary
    └── checksums-sha256.txt
```

### Self-Update (Flatline Updating Itself)

This is the classic "binary replacing itself" problem. The approach:

1. Flatline replaces its own binary on disk (the running process
   keeps the old file descriptor open — Unix allows this)
2. Flatline exits with a special code (exit code 10)
3. systemd restarts the service → new binary starts
4. New Flatline runs a self-check: can it read config, connect to
   logs, access state.db?
5. If self-check fails: the binary is already replaced, so systemd
   will keep restarting the broken version. The old binary is at
   `flatline.prev` — the user (or a cron job) can restore it.

```ini
# systemd addition for clean self-update
[Service]
RestartForceExitStatus=10    # treat exit 10 as "please restart me"
```

Flatline ONLY self-updates after Wintermute's update is confirmed
healthy. If Wintermute's update fails and gets rolled back, Flatline
does NOT update itself (versions might be coupled).

### What Triggers an Update

| Trigger | Behavior |
|---------|----------|
| Daily timer (04:00) | Check + notify (or auto-apply) |
| User sends `/check-update` | Check immediately, report result |
| User sends `/update` | Apply pending update now |
| User sends `/update-now` | Apply even if not idle |
| User sends `/skip` | Defer this version, check again tomorrow |
| User sends `/pin` | Stay on current version until `/unpin` |
| Flatline restart | Check if interrupted update needs cleanup |

### Migration Support

Some updates need more than a binary swap. A release can include a
migration script that runs between steps 4 and 5:

```
Release v0.5.0:
  assets:
    - wintermute-0.5.0-x86_64-unknown-linux-gnu.tar.gz
    - flatline-0.5.0-x86_64-unknown-linux-gnu.tar.gz
    - checksums-sha256.txt
    - migrate-0.4-to-0.5.sh    ← optional migration
```

Migration scripts run with limited scope:
- Can modify files in `~/.wintermute/` (config, db schema)
- Cannot modify config.toml (security policy stays human-only)
- Cannot access the network
- Run AFTER the binary is swapped but BEFORE Wintermute starts
- Logged in full to `updates/last_update.json`
- If migration fails: rollback the binary too

Example migrations:
- Database schema changes (ALTER TABLE in memory.db/state.db)
- Config format changes (add new fields to agent.toml)
- Rename directories
- Convert log format

### Version Pinning & Compatibility

```rust
// In both binaries
const MIN_COMPATIBLE_FLATLINE: &str = "0.3.0";
const MIN_COMPATIBLE_WINTERMUTE: &str = "0.3.0";
```

On startup, each binary checks the other's version:
- Wintermute reads Flatline's version from `flatline --version`
- Flatline reads Wintermute's version from `wintermute --version`
- If incompatible: alert user, refuse to start (don't silently break)

This prevents partial updates from causing mysterious failures.

### Docker Image Updates

Docker images (sandbox + browser sidecar) are pulled before the binary
swap. This means:

1. `docker pull` can happen while Wintermute is still running
2. Old containers keep running on old images until restart
3. After binary swap, Wintermute's startup recreates the sandbox
   container from the new image (runs setup.sh on the new base)
4. Browser sidecar uses the new image on next launch (on-demand)

If `docker pull` fails (network, registry down), the update is deferred.
Binaries are NOT swapped without their matching images.

Image tags follow the release version: `ghcr.io/{owner}/wintermute-sandbox:v0.4.0`.
The `latest` tag also moves, but Flatline always pulls the specific
version tag for reproducibility.

---

## What Flatline Cannot Do

- **Cannot modify config.toml** — security policy is human-only
- **Cannot delete tools** — only quarantine (reversible) or revert
- **Cannot modify memory.db** — read-only access
- **Cannot change budget limits** — that's config.toml
- **Cannot approve things on behalf of the user** — only Wintermute's
  approval flow handles that
- **Cannot install packages** — that's Wintermute's job (via sandbox)
- **Cannot access the sandbox** — no Docker interaction (except image pulls for updates)
- **Cannot modify AGENTS.md** — that's the agent's document

Flatline is deliberately less powerful than Wintermute. It can roll back,
restart, quarantine, disable, update binaries/images, and report. It
cannot create, install, configure, or grant permissions.

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
│   │   ├── meta_reader.rs  # Read _meta from /scripts/*.json
│   │   ├── patterns.rs     # Known failure pattern matching
│   │   ├── diagnosis.rs    # LLM-based diagnosis (novel problems)
│   │   ├── fixer.rs        # Fix proposal + application + verification
│   │   ├── reporter.rs     # Telegram notifications + daily reports
│   │   ├── updater.rs      # Auto-update: check, download, swap, rollback
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
RestartForceExitStatus=10
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
default = "anthropic/claude-opus-4-6"
# fallback = "ollama/qwen3:8b"

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
escalation_alert_daily = 10        # alert if >10 oracle calls per day
soul_regression_window_hours = 24  # monitor behavior for 24h after soul change

[auto_fix]
enabled = true
restart_on_crash = true            # auto-restart Wintermute
quarantine_failing_tools = true    # auto-quarantine after threshold
disable_failing_tasks = true       # auto-disable after 3 consecutive failures
revert_recent_changes = true       # auto-revert if correlated with failure
revert_bad_setup = true            # auto-revert setup.sh if container fails
max_auto_restarts_per_hour = 3     # stop retrying after N restarts

[reports]
daily_health = "08:00"             # daily health summary
alert_cooldown_mins = 30           # don't repeat same alert within window
telegram_prefix = "🩺 Flatline"
include_tool_trends = true         # include tool health trends in daily report

[update]
enabled = true                     # check for updates
channel = "stable"                 # stable | nightly
check_time = "04:00"               # daily check time (local)
auto_apply = false                 # true = update without asking, false = notify + wait for /update
idle_patience_hours = 6            # how long to wait for idle before nagging
health_watch_secs = 300            # monitor health for 5 min after update
repo = "pycckuu/wintermute"        # GitHub owner/repo
# pinned_version = "0.3.2"         # uncomment to pin to specific version

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
- [x] Tool _meta in dynamic tool schemas
- [x] setup.sh for sandbox dependency persistence
- [x] soul_modified, escalation, no_reply log events

### Flatline Build Order

**Week 1: Foundation**
- Scaffold: CLI, config loading, process management
- Log watcher: tail .jsonl files, parse events
- Health file monitor: detect stale health.json
- Meta reader: parse _meta from /scripts/*.json
- Stats engine: rolling window per-tool success/failure rates
  (from both logs and _meta)

**Week 2: Patterns + Fixes**
- Pattern matcher: all known patterns from the design
- Fix proposer: generate fix records from pattern matches
- Fix applier: git revert, quarantine, restart, disable task,
  revert setup.sh + reset sandbox
- Verification: check if fix worked after application

**Week 3: Reporting + LLM**
- Telegram reporter: alerts, proposals, daily health (with tool
  trends and soul change tracking)
- LLM diagnosis: novel problem analysis via cheap model
- Approval flow: propose → user approves → apply
- State database: tool stats, fix history, suppressions,
  soul change tracking

**Week 4: Auto-Update**
- GitHub Releases API client: check latest, compare semver
- Download + SHA256 verification
- Binary swap: backup → replace → restart → health watch
- Rollback: detect unhealthy post-update, restore .prev
- Self-update: replace on disk, exit with code 10
- Docker image pulls: sandbox + browser sidecar
- Migration script support: run between swap and restart
- Telegram commands: /update, /skip, /pin, /check-update

**Week 5: Hardening**
- Edge cases: Flatline crashes during fix or update application
- Idempotency: same fix not applied twice
- Interrupted update recovery: detect partial state on restart
- Cooldowns: don't spam alerts, don't restart in a loop
- Version compatibility checks between binaries
- Testing: simulate failure scenarios + failed update rollbacks

---

## Future Extensions (v1.2+)

**Predictive health:** Detect trends before they become failures.
Tool X's latency has been increasing 10% per week — investigate.
Compare _meta snapshots across git history for trend analysis.

**Automated testing:** After reverting a tool, run the tool with
a sample input to verify it works before declaring it fixed.

**Multi-agent coordination:** If multiple Wintermute instances exist
(different users/machines), Flatline aggregates health across fleet.

**Self-improvement suggestions:** "Tool X is called 50 times/day
but takes 30 seconds each time. You could optimize it by caching
the API response." Based on _meta analysis.

**Anomaly detection:** Learn normal behavior patterns, alert on
deviations without predefined rules. Requires more historical data.

**AGENTS.md integration:** When Flatline diagnoses and fixes a novel
issue, propose a new entry for AGENTS.md so the agent learns from
the failure pattern.