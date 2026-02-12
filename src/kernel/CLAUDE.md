# Kernel — Trusted Computing Base

The kernel is the security core of PFAR. All code here runs in the trusted boundary (spec section 5).

## Module Map

| Module | Spec | Status | Purpose |
|---|---|---|---|
| router | 6.1 | Done | Receives InboundEvent, resolves principal, assigns labels, dispatches to pipeline |
| policy | 6.2 | Done | Label assignment, propagation (max), No Read Up, No Write Down, taint checking, capabilities |
| template | 4.5 | Done | TOML task templates, trigger matching, TemplateRegistry |
| inference | 6.3, 11.1 | Done | Multi-provider proxy (Ollama, OpenAI, Anthropic, LM Studio), label-based routing |
| vault | 6.4 | Done | Secret storage abstraction (InMemoryVault for Phase 1, SQLCipher Phase 2) |
| audit | 6.7 | Done | Structured JSON audit log, append-only |
| planner | 7, 10.3, 13.3 | Done | Phase 1 prompt composition, plan parsing, plan validation |
| synthesizer | 7, 10.7, 13.4 | Done | Phase 3 prompt composition from tool results |
| executor | 7, 10.4, 10.6 | Done | Phase 2 plan executor, tool dispatch with policy enforcement |
| egress | 10.8, 14.6 | Done | Egress validation, No Write Down enforcement, audit logging |
| scheduler | 6.5 | Skeleton | Cron jobs with explicit template per job |
| approval | 6.6 | Done | Human approval queue: submit/resolve/timeout with oneshot channels |
| container | 6.8 | Skeleton | Podman container lifecycle, gVisor sandbox, GC within 30s TTL |
| pipeline | 7 | Done | Plan-Then-Execute: Extract → Plan → Execute → Synthesize |
| session | 9.1, 9.2 | Done | Per-principal working memory and conversation history |

## Trust Boundary Rules

- Kernel code MUST enforce all 10 privacy invariants
- Label propagation: always use `max(current_label, new_label)` — labels never decrease
- Taint propagation: raw external data starts as `TaintLevel::Raw`
- Capability tokens: short-lived, single-task, scoped to specific tool+resource
- No LLM in the kernel's trusted path sees both raw content AND has tool access

## Key Types (from src/types/mod.rs)

Reuse these — do NOT recreate: `Principal`, `PrincipalClass`, `SecurityLabel`, `TaintLevel`, `TaintSet`, `CapabilityToken`, `Task`, `TaskState`, `InboundEvent`, `LabeledEvent`, `ApprovalDecision`, `ToolResult`
