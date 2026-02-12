# Kernel — Trusted Computing Base

The kernel is the security core of PFAR. All code here runs in the trusted boundary (spec section 5).

## Module Map

| Module | Spec | Purpose |
|---|---|---|
| router | 6.1 | Receives InboundEvent, resolves principal, assigns labels, dispatches to pipeline |
| policy | 6.2 | Label assignment, propagation (max), No Read Up, No Write Down, taint checking |
| inference | 6.3 | HTTP proxy to Ollama (localhost), label-based routing for cloud LLMs |
| vault | 6.4 | OS keychain for master key, SQLCipher for credentials/sessions/audit DBs |
| scheduler | 6.5 | Cron jobs with explicit template per job |
| approval | 6.6 | Human approval queue for tainted writes |
| audit | 6.7 | Structured JSON audit log, append-only |
| container | 6.8 | Podman container lifecycle, gVisor sandbox, GC within 30s TTL |
| pipeline | 7 | Plan-Then-Execute: Extract → Plan → Execute → Synthesize |

## Trust Boundary Rules

- Kernel code MUST enforce all 10 privacy invariants
- Label propagation: always use `max(current_label, new_label)` — labels never decrease
- Taint propagation: raw external data starts as `TaintLevel::Raw`
- Capability tokens: short-lived, single-task, scoped to specific tool+resource
- No LLM in the kernel's trusted path sees both raw content AND has tool access

## Key Types (from src/types/mod.rs)

Reuse these — do NOT recreate: `Principal`, `PrincipalClass`, `SecurityLabel`, `TaintLevel`, `TaintSet`, `CapabilityToken`, `Task`, `TaskState`, `InboundEvent`, `LabeledEvent`, `ApprovalDecision`, `ToolResult`
