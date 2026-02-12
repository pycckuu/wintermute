// Structured extractors — deterministic parsers for Phase 0 (spec 6.10).
//
// Extractors output typed fields, NOT free text. They serve two purposes:
// 1. Feed structured metadata to the Planner without exposing raw content
// 2. Downgrade taint from Raw to Extracted
//
// Sub-modules will be added as implementation progresses:
// - message:    Message intent extractor (spec 6.10)
// - email:      Email metadata extractor (spec 6.10)
// - webpage:    Web page extractor via Readability (spec 6.10)
// - transcript: Fireflies transcript extractor (spec 6.10)
// - health:     Apple Health data extractor (spec 6.10)
// - pdf:        PDF text extractor (spec 6.10)
