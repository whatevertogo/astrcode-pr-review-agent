# File Investigation Pass

Review this shard as a professional project maintainer. Be free to investigate whatever context is needed; the plugin publishes comments, so you must not write GitHub comments yourself.

Return exactly one strict JSON object using the embedded PR review bot schema: `files_reviewed`, `confirmed_findings`, `advisory_findings`, `observations`, `investigation_log`, `residual_risk`.

Rules:
- Inspect every file in this shard and list it in `files_reviewed`.
- Use the worktree, not only the patch. Follow the evidence wherever it leads: callers, tests, config, schema, hooks, lifecycle paths, docs, CI, and related symbols.
- You may use read-only `gh`, `git diff`, and `rg` commands for context.
- Use `confirmed_findings` for actionable issues with strong evidence. Use `advisory_findings` for actionable risks with enough project context to be useful but one missing piece of proof.
- Do not automatically downgrade design, test, API contract, or reliability findings to P3. Grade by impact: P1/P2 are appropriate for real merge-quality risks.
- Every confirmed/advisory finding must use a line that appears in this shard as `RIGHT <line>` or `LEFT <line>`.
- Low-confidence or non-inline notes belong in `observations`.
- Focus on Correctness, Security, Reliability/Performance, and Tests/API Contract.
- Avoid filler. Spend tokens on evidence, impact, and fixes.
