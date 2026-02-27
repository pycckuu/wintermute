CREATE TABLE IF NOT EXISTS task_briefs (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    contact_id INTEGER,
    objective TEXT NOT NULL,
    shareable_info TEXT NOT NULL,
    constraints TEXT NOT NULL,
    escalation_triggers TEXT,
    commitment_level TEXT NOT NULL
        CHECK(commitment_level IN ('can_commit', 'negotiate_only', 'information_only')),
    tone TEXT,
    status TEXT NOT NULL DEFAULT 'draft'
        CHECK(status IN ('draft', 'confirmed', 'active', 'escalated',
                         'proposed', 'committed', 'completed', 'cancelled')),
    outcome_summary TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    completed_at TEXT
);

CREATE TABLE IF NOT EXISTS contacts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    phone TEXT,
    whatsapp_jid TEXT,
    organization TEXT,
    notes TEXT,
    last_contacted_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS outbound_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    brief_id TEXT,
    session_id TEXT NOT NULL,
    channel TEXT NOT NULL,
    recipient TEXT NOT NULL,
    message_text TEXT NOT NULL,
    direction TEXT NOT NULL CHECK(direction IN ('outbound', 'inbound')),
    redaction_warnings TEXT,
    blocked BOOLEAN DEFAULT FALSE,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_briefs_session ON task_briefs(session_id);
CREATE INDEX IF NOT EXISTS idx_briefs_status ON task_briefs(status);
CREATE INDEX IF NOT EXISTS idx_outbound_brief ON outbound_log(brief_id);
CREATE INDEX IF NOT EXISTS idx_contacts_name ON contacts(name);
CREATE INDEX IF NOT EXISTS idx_contacts_jid ON contacts(whatsapp_jid);
