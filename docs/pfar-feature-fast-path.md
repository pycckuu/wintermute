# PFAR Feature Spec: Pipeline Fast Path

> **Feature**: Skip Planner for messages that don't need tools  
> **Status**: Implementation-ready  
> **Priority**: Phase 2  
> **Complexity**: Small — one boolean check in the pipeline, one config guideline

---

## Problem

Every message goes through the full 4-phase pipeline including an LLM call to the Planner. "Hey!" takes 40 seconds because the Planner spends ~34 seconds reasoning about whether tools are needed, then returns an empty plan.

---

## Solution

After Phase 0 (Extract), check if planning is needed. If not, skip Phase 1 and 2, go straight to Synthesize.

```rust
// In run_pipeline(), after Phase 0 extract:

let needs_tools = extracted.intent.is_some()
    && template.allowed_tools.iter().any(|t| extracted.could_use(t));

if needs_tools {
    let plan = self.plan(task, &extracted).await?;
    let results = self.execute(task, &plan).await?;
    self.synthesize(task, results, &extracted).await?;
} else {
    self.synthesize(task, vec![], &extracted).await?;
}
```

One boolean. No new enums, no classifier module, no model routing logic.

---

## Privacy Impact

None. The fast path skips the Planner (which only produces a JSON plan) and the Executor (which only calls tools). The Synthesizer still runs through the inference proxy with the same data ceiling checks, label-based LLM routing, sink validation, and taint rules. All kernel enforcement is on the egress path, which both routes share.

---

## Model Guidance

Don't use reasoning models (R1, o1) for pipeline inference. They produce `<think>` blocks that multiply latency without improving plan or response quality. Use instruction-tuned models instead.

If the owner wants reasoning for specific tasks (deep analysis, code review), create a dedicated template with a reasoning model and an explicit trigger:

```toml
# Default — fast model for everyday use
# ~/.pfar/templates/owner_telegram_general.toml
[inference]
provider = "lmstudio"
model = "qwen3-8b"

# Deep analysis — reasoning model, owner triggers explicitly
# ~/.pfar/templates/owner_deep_analysis.toml
triggers = ["adapter:telegram:command:analyze"]
[inference]
provider = "lmstudio"
model = "deepseek-r1-0528-qwen3-8b"
```

This already works with the existing template system. No new feature needed.

---

## Expected Latency

| Message | Before | After |
|---|---|---|
| "Hey!" | ~40s | ~3s |
| "What's the capital of France?" | ~40s | ~3s |
| "Check my email" | ~40s | ~6-7s |
| "/analyze this PR" (reasoning template) | ~40s | ~40s (intentional) |

---

## Implementation Checklist

- [ ] Add `could_use(tool)` method to `ExtractedMetadata` — checks if extracted intent/entities match a tool's domain
- [ ] Add fast path branch in `run_pipeline()` (the `if needs_tools` check above)
- [ ] Log which path was taken: `pipeline_path=fast` or `pipeline_path=full`
- [ ] Switch default model in config from R1 to non-reasoning variant
- [ ] Create `owner_deep_analysis.toml` template with reasoning model for explicit use
- [ ] Test: "Hey!" completes in under 5 seconds
- [ ] Test: "Check my email" still runs full pipeline with tools
