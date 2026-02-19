---
name: review-wintermute-pr
description: Wintermute-specialized local code review with 4 parallel agents.
---

Follow the workflow defined in `dev/skills/review-wintermute-pr.md`.
Read that file now and execute it.

All four review agents are mandatory. Do not return a partial review.
Flag any `#[cfg(test)]` usage under `src/` as a test placement policy violation.
