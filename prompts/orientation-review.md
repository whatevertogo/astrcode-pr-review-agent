# PR Orientation Pass

Read the PR metadata, changed-file manifest, checks, repository memory, and related PR/issue reminders. Return strict JSON using the embedded PR review bot schema.

Purpose:
- Identify the PR intent and the areas that need deeper investigation.
- Surface related PR/issue reminders as `observations` when they may matter to this review.
- Add concise `investigation_log` entries that tell later passes what context is important.
- Do not create confirmed/advisory findings unless the metadata itself proves a concrete diff-line issue.

Rules:
- Do not post GitHub comments.
- You may use read-only `gh`, `git diff`, and `rg` for orientation.
- Keep findings empty unless you can cite a valid diff line from the annotated PR context.
- Use `observations` for repo history, previous review memory, likely risky subsystems, and follow-up questions.
- Every `observations` item must be a JSON object, not a string.
- Keep `residual_risk` only for real blockers such as missing PR metadata, unavailable file manifests, or inaccessible checks.
