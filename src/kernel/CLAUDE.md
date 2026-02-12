# Kernel — Trusted Computing Base

The kernel is the security core of PFAR. All code here runs in the trusted boundary (spec section 5).

## Module Map

| Module | Spec | Status | Purpose |
|---|---|---|---|
| router | 6.1 | Done | Receives InboundEvent, resolves principal, assigns labels, dispatches to pipeline |
| policy | 6.2 | Done | Label assignment, propagation (max), No Read Up, No Write Down, taint checking, capabilities |
| template | 4.5 | Done | TOML task templates, trigger matching, TemplateRegistry |
| inference | 6.3 | Done | HTTP proxy to Ollama (localhost), label-based routing for cloud LLMs |
| vault | 6.4 | Done | Secret storage abstraction (InMemoryVault for Phase 1, SQLCipher Phase 2) |
| audit | 6.7 | Done | Structured JSON audit log, append-only |
| scheduler | 6.5 | Skeleton | Cron jobs with explicit template per job |
| approval | 6.6 | Skeleton | Human approval queue for tainted writes |
| container | 6.8 | Skeleton | Podman container lifecycle, gVisor sandbox, GC within 30s TTL |
| pipeline | 7 | Skeleton | Plan-Then-Execute: Extract → Plan → Execute → Synthesize |

## Trust Boundary Rules

- Kernel code MUST enforce all 10 privacy invariants
- Label propagation: always use `max(current_label, new_label)` — labels never decrease
- Taint propagation: raw external data starts as `TaintLevel::Raw`
- Capability tokens: short-lived, single-task, scoped to specific tool+resource
- No LLM in the kernel's trusted path sees both raw content AND has tool access

## Key Types (from src/types/mod.rs)

Reuse these — do NOT recreate: `Principal`, `PrincipalClass`, `SecurityLabel`, `TaintLevel`, `TaintSet`, `CapabilityToken`, `Task`, `TaskState`, `InboundEvent`, `LabeledEvent`, `ApprovalDecision`, `ToolResult`
