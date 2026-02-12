// Tool modules — in-process SaaS integrations (spec 5.4, 6.10, 6.11).
//
// Each tool implements the Tool trait and receives only what the
// kernel provides: a validated capability token, injected credentials,
// a domain-scoped HTTP client, and validated arguments.
//
// Sub-modules will be added as implementation progresses:
// - admin:      Conversational configuration (spec 8)
// - email:      Zoho Mail / Gmail (spec 6.11)
// - calendar:   Google Calendar (spec 6.11)
// - github:     GitHub API (spec 6.11)
// - notion:     Notion API (spec 6.11)
// - bluesky:    Bluesky API (spec 6.11)
// - twitter:    Twitter/X API (spec 6.11)
// - fireflies:  Fireflies API (spec 6.11)
// - cloudflare: Cloudflare API (spec 6.11)
// - http:       Generic HTTP (spec 6.11)
