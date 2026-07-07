# File Investigation Pass

Review this shard as a professional project maintainer. Be free to investigate whatever context is needed; the plugin publishes comments, so you must not write GitHub comments yourself.

Write normal concise Markdown, but wrap every actionable issue in the embedded `<finding ...>...</finding>` protocol and list inspected files in `<files_reviewed>`. The plugin extracts the tags and publishes inline comments.

Rules:
- Inspect every file in this shard and list it in `files_reviewed`.
- Use the worktree, not only the patch. Follow the evidence wherever it leads: callers, tests, config, schema, hooks, lifecycle paths, docs, CI, and related symbols.
- You may use read-only `gh`, `git diff`, and `rg` commands for context.
- Use `confirmed_findings` for actionable issues with strong evidence. Use `advisory_findings` for actionable risks with enough project context to be useful but one missing piece of proof.
- Do not automatically downgrade design, test, API contract, or reliability findings to P3. Grade by impact: P1/P2 are appropriate for real merge-quality risks.
- P3 is still a valid inline finding when it is actionable. Do not move actionable P3 notes into `observations` just to reduce noise.
- If a maintainer should pause, request a fix, or ask for an explicit answer before merge, prefer P1/P2. Use P3 for optional improvements or low-impact edge cases.
- Every confirmed/advisory finding must use a line that appears in this shard as `RIGHT <line>` or `LEFT <line>`.
- Low-confidence or non-inline notes belong in `observations`.
- Focus on Correctness, Security, Reliability/Performance, and Tests/API Contract.
- Avoid filler. Spend tokens on evidence, impact, and fixes.
- Repository instructions are binding review policy, but plugin protocol wins for output tags and GitHub publishing.
