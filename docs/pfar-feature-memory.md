# PFAR Feature Spec: Memory System

> **Feature**: Persistent, searchable, privacy-safe agent memory  
> **Status**: Implementation-ready  
> **Priority**: Phase 3  
> **Depends on**: Pipeline (Phase 2), Vault (Phase 1)

---

## 1. Problem

PFAR's session working memory holds the last 10 structured task results. Enough for "reply to that email" but not for "what did we talk about last week?" or "remember I'm going to Bali in March." The agent has no long-term memory.

---

## 2. Design

The kernel controls all memory access. Agents never search or write memory directly.

Two write paths:
- **Explicit save**: user says "remember this" → Planner adds `memory.save` to the plan → kernel executes it
- **Daily consolidation**: cron summarizes yesterday's conversations into durable entries

One read path:
- **Kernel injection**: before every pipeline run, the kernel searches memory and injects relevant results into Planner and Synthesizer prompts

---

## 3. Schema

In `memory.db` (existing SQLCipher database):

```sql
CREATE TABLE memories (
    id          TEXT PRIMARY KEY,
    content     TEXT NOT NULL,
    label       TEXT NOT NULL DEFAULT 'internal',
    source      TEXT NOT NULL,      -- "explicit" or "consolidated"
    created_at  TEXT NOT NULL,
    task_id     TEXT
);

CREATE VIRTUAL TABLE memories_fts USING fts5(
    content,
    content='memories',
    content_rowid='rowid'
);

CREATE TRIGGER memories_ai AFTER INSERT ON memories BEGIN
    INSERT INTO memories_fts(rowid, content)
    VALUES (new.rowid, new.content);
END;

CREATE TRIGGER memories_ad AFTER DELETE ON memories BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, content)
    VALUES ('delete', old.rowid, old.content);
END;
```

---

## 4. Explicit Save

The `memory.save` tool, available in task templates:

```rust
Tool {
    name: "memory.save",
    semantics: Write,
    target: Internal,  // writes to local vault, not external — no approval needed
    args: { content: String }
}
```

The Planner recognizes memory intent and includes it in the plan:

```
User: "Remember that my flight to Bali is March 15th"
→ Phase 0: Extract {intent: "memory_save", entities: ["Bali", "March 15th"]}
→ Phase 1: Plan [{tool: "memory.save", args: {content: "Flight to Bali on March 15th"}}]
→ Phase 2: Kernel writes to memory.db, label = task's data ceiling
→ Phase 3: "Got it."
```

---

## 5. Daily Consolidation

Cron job at 04:00 daily:

```rust
pub async fn consolidate_memories(&self) -> Result<()> {
    // 1. Load yesterday's completed tasks from sessions.db
    let tasks = self.vault.sessions_db.get_tasks_for_date(yesterday())?;
    if tasks.is_empty() { return Ok(()); }

    // 2. Build conversation summaries (structured data, not raw messages)
    let summaries: Vec<String> = tasks.iter()
        .map(|t| format!("{}: {}", t.template_id, t.result_summary))
        .collect();

    // 3. LLM call: extract durable facts
    //    Label-aware routing: if any task was sensitive/regulated → local LLM only
    let max_label = tasks.iter().map(|t| &t.data_ceiling).max();
    let prompt = format!(
        "Extract facts worth remembering long-term from yesterday's activity.\n\
         Output one fact per line. Skip trivial interactions (greetings, chitchat).\n\
         If something contradicts a previous fact, state the updated version.\n\n\
         Activity:\n{}",
        summaries.join("\n")
    );
    let response = self.inference_proxy.complete(&prompt, max_label).await?;

    // 4. Store each line as a memory entry
    for line in response.lines().filter(|l| !l.trim().is_empty()) {
        self.vault.memory_db.insert_memory(Memory {
            content: line.trim().to_string(),
            label: max_label.clone(),
            source: "consolidated".into(),
        })?;
    }

    Ok(())
}
```

The consolidation handles dedup and contradictions naturally. If the user changed their flight from March 15 to March 20, the LLM produces "User's flight to Bali is March 20 (changed from March 15)." Both the old explicit entry and the new consolidated one exist in the DB. Search returns both, the Synthesizer sees timestamps, picks the newer one.

---

## 6. Kernel Search and Injection

After Phase 0 (Extract), the kernel searches memory and adds results to the prompt context:

```rust
pub fn search_memories(
    extracted: &ExtractedMetadata,
    data_ceiling: &SecurityLabel,
) -> Vec<Memory> {
    // Build search query from extracted entities and keywords
    let terms: Vec<&str> = extracted.entities.iter()
        .map(|e| e.value.as_str())
        .chain(extracted.keywords.iter().map(|k| k.as_str()))
        .collect();

    if terms.is_empty() {
        return vec![];
    }

    let fts_query = terms.join(" OR ");

    memory_db.query(
        "SELECT id, content, label, source, created_at FROM memories
         WHERE rowid IN (SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?)
         AND label <= ?
         ORDER BY rank
         LIMIT 10",
        [&fts_query, data_ceiling]
    )
}
```

Results are injected into both prompts as a simple section:

```
## Relevant Memory
- Flight to Bali on March 15th (Feb 10)
- Prefers concise responses, no emoji (Feb 8)
- Working on PFAR v2, Rust project (Feb 13)
```

If no search terms are extracted or no results found, the section is omitted. The agents don't know memory exists — they just see context.

**Label enforcement**: `label <= data_ceiling` ensures a public-ceiling task never sees sensitive memories. This is the core privacy advantage over OpenClaw where MEMORY.md is all-or-nothing.

---

## 7. Memory Query Handling

When the user asks "what do you remember about Bali?":

```
User: "What do you remember about my Bali trip?"
→ Phase 0: Extract {entities: ["Bali", "trip"]}
→ Kernel memory search: finds entries about Bali
→ Fast path (no tools needed) → Synthesizer
→ Synthesizer sees memory results in context
→ Response: "You have a flight to Bali on March 15th."
```

No special tool needed. The standard kernel search surfaces the entries, the fast path handles it.

---

## 8. Implementation Checklist

- [ ] `memories` table + `memories_fts` virtual table in memory.db
- [ ] FTS5 sync triggers (insert/delete)
- [ ] `memory.save` tool definition and registration
- [ ] Intent detection: "remember this" / "don't forget" patterns in extractor
- [ ] `search_memories()` — FTS5 query with label filtering
- [ ] Inject memory results into Planner prompt
- [ ] Inject memory results into Synthesizer prompt
- [ ] Daily consolidation cron job (04:00)
- [ ] Consolidation uses label-aware LLM routing (sensitive → local only)
- [ ] Test: "remember X" creates entry, later query finds it
- [ ] Test: label enforcement — `internal` task cannot see `sensitive` memory
- [ ] Test: consolidation produces entries from yesterday's tasks
- [ ] Test: "what do you know about X?" returns relevant memories
