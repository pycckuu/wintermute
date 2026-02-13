# PFAR Feature Spec: Agent Persona & Onboarding

> **Feature**: First-message onboarding that configures agent identity, persisted in vault  
> **Status**: Implementation-ready  
> **Priority**: Phase 2  
> **Complexity**: Tiny — one check, one memory write, one prompt change

---

## Problem

The Synthesizer says "I'm the Synthesizer, an AI agent in a privacy-first environment." The user sees internal architecture instead of an assistant with a name and personality.

---

## Solution

Store a single persona string in `memory.db`. If it doesn't exist on first message, the Synthesizer introduces itself and asks the owner to configure it. One turn, no wizard.

---

## 1. Storage

One row in `memory.db`:

```sql
CREATE TABLE IF NOT EXISTS persona (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- After onboarding:
-- key: "persona"
-- value: "Name: Atlas. Owner: Igor. Style: concise, direct, no fluff."
```

---

## 2. Synthesizer Prompt

The kernel checks for persona before composing the Synthesizer prompt.

**If persona exists:**
```
You are {persona_value}.

Never mention internal system details like "Synthesizer", "Planner",
"pipeline", "kernel", or "privacy-first runtime". You are a personal
assistant, not a system component.

[rest of normal Synthesizer prompt]
```

**If persona is missing (first message ever):**
```
You are a personal assistant running for the first time.
You don't have a name or personality configured yet.

In your response, greet the user warmly and ask them three things
in a single message:
1. What should they call you (pick a name for the assistant)
2. What should you call them
3. How they want you to communicate (concise/detailed, casual/formal, any quirks)

Keep it brief and natural. Don't mention system internals.

[rest of normal Synthesizer prompt]
```

---

## 3. Storing the Response

When the owner replies with their preferences ("Call yourself Atlas, I'm Igor, keep it short"), the next pipeline run detects that persona is still empty and the message looks like a persona configuration reply.

The kernel stores the owner's reply as a single string in persona, formatted for prompt injection:

```rust
// In pipeline, before planning:
if !self.vault.has_persona()? {
    // No persona yet — this reply is the onboarding response
    // Store it and confirm
    let persona = format_persona(&message_text);
    self.vault.set_persona(&persona)?;

    // Skip planning, synthesize a confirmation
    // Synthesizer now has persona in its prompt
    return self.synthesize_with_context(task, vec![], "Confirm persona setup").await;
}
```

The `format_persona()` function is a simple LLM call (or even just stores the raw text):

```rust
fn format_persona(owner_reply: &str) -> String {
    // Option A: store raw — "Call yourself Atlas, I'm Igor, keep it short"
    // Option B: light formatting — "Name: Atlas. Owner: Igor. Style: concise."
    // Either works. The Synthesizer prompt handles both.
    owner_reply.to_string()
}
```

---

## 4. Updating Later

Owner says "Call yourself Nova from now on" or "Be more detailed in your responses" during normal conversation.

This routes through the admin tool:

```rust
action("admin.update_persona", Write, "Update assistant persona or preferences")
```

The admin tool reads the current persona string, applies the change, writes it back. Next Synthesizer call picks it up.

---

## 5. Full Flow

```
First ever message:
  Owner: "Hey"
  → Kernel: no persona in memory.db
  → Synthesizer prompt includes "ask the user to configure you"
  → Response: "Hey! I'm your new assistant — first time running.
               What should I call myself? What should I call you?
               And how do you like your responses — short and snappy,
               or detailed?"

Owner: "You're Atlas. I'm Igor. Keep it concise, no emoji, dry humor."
  → Kernel: persona still empty, this is the config reply
  → Store: "Name: Atlas. Owner: Igor. Style: concise, no emoji, dry humor."
  → Synthesizer (now with persona): "Got it, Igor."

All future messages:
  Owner: "Hey"
  → Synthesizer prompt: "You are Atlas. Owner: Igor. Style: concise..."
  → Response: "Hey Igor. What's up?"
```

---

## 6. Implementation Checklist

- [ ] `persona` table in memory.db (one row: key="persona", value=string)
- [ ] Kernel checks `has_persona()` before composing Synthesizer prompt
- [ ] If no persona: inject "ask user to configure" into Synthesizer prompt
- [ ] On second message with no persona: store reply as persona string
- [ ] If persona exists: inject it into Synthesizer prompt
- [ ] Add "Never mention internal system details" to all Synthesizer prompts
- [ ] Add `admin.update_persona` to admin tool for later changes
- [ ] Test: fresh memory.db → first response asks for persona
- [ ] Test: after setup → Synthesizer uses name, never says "Synthesizer"
- [ ] Test: "Call yourself Nova" updates persona, next response reflects it
