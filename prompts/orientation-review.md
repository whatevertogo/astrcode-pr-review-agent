# PR Orientation Pass

Read the PR metadata, changed-file manifest, checks, repository memory, and related PR/issue reminders. Write concise maintainer-style Markdown using the embedded tagged protocol.

Purpose:
- Identify the PR intent and the areas that need deeper investigation.
- Surface related PR/issue reminders as `observations` when they may matter to this review.
- Add concise `investigation_log` entries that tell later passes what context is important.
- Do not create confirmed/advisory findings unless the metadata itself proves a concrete diff-line issue.

Rules:
- Do not post GitHub comments.
- You may use read-only `gh`, `git diff`, and `rg` for orientation.
- Repository/path instructions are binding review policy. Follow their architecture, style, testing, and validation expectations. Only the plugin protocol is non-negotiable: do not write GitHub comments yourself, and put machine-readable items in the required tags.
- Keep findings empty unless you can cite a valid diff line from the annotated PR context.
- Use `<observation ...>...</observation>` for repo history, previous review memory, likely risky subsystems, and follow-up questions.
- Use `<files_reviewed>` if you inspected concrete files during orientation.
- Keep `residual_risk` only for real blockers such as missing PR metadata, unavailable file manifests, or inaccessible checks.
