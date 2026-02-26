CREATE TABLE IF NOT EXISTS tool_stats (
    tool_name TEXT NOT NULL,
    window_start TEXT NOT NULL,
    success_count INTEGER NOT NULL DEFAULT 0,
    failure_count INTEGER NOT NULL DEFAULT 0,
    avg_duration_ms INTEGER,
    PRIMARY KEY (tool_name, window_start)
);

CREATE TABLE IF NOT EXISTS fixes (
    id TEXT PRIMARY KEY,
    detected_at TEXT NOT NULL,
    pattern TEXT,
    diagnosis TEXT,
    action TEXT,
    applied_at TEXT,
    verified INTEGER,
    user_notified INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS suppressions (
    pattern TEXT PRIMARY KEY,
    suppressed_until TEXT,
    reason TEXT
);

CREATE TABLE IF NOT EXISTS updates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    checked_at TEXT NOT NULL,
    from_version TEXT NOT NULL,
    to_version TEXT NOT NULL,
    status TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT,
    rollback_reason TEXT,
    migration_log TEXT
);
