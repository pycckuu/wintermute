---
name: update-claude-config
description: Review and update Claude Code configuration files after feature implementation
---

# Update Claude Config After Feature Implementation

After implementing a feature, review whether Claude Code config files need updates.

## Check Each File

### 1. `CLAUDE.md` (project root)
- New build commands or workflow steps?
- New code rules or conventions introduced?
- New dependencies that affect how code is written?

### 2. `src/kernel/CLAUDE.md`
- New kernel sub-module added? Update the module map table
- New trust boundary rules or privacy implications?

### 3. `docs/pfar-v2-prd.md`
- Mark completed tasks with `- [x]`
- Add any newly discovered sub-tasks

### 4. `.claude/agents/security-reviewer.md`
- New privacy invariants or threat vectors to review against?

### 5. `.claude/skills/implement-kernel-module/SKILL.md`
- New implementation steps or patterns established?

### 6. MEMORY.md (persistent memory)
- Update "Current State" section if phase progress changed
- Add new conventions discovered during implementation
- Add new dependencies if added to Cargo.toml

## Rules
- Only update files where something actually changed
- Keep all files concise — remove outdated info, don't just append
- Do NOT update files if nothing relevant changed — report "no updates needed"
