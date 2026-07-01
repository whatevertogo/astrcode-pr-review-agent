# Aggregate Review Pass

Aggregate previous JSON outputs. The plugin will validate line locations and publish comments; you must not call GitHub APIs.

Return exactly one strict JSON object with the embedded PR review bot schema: `files_reviewed`, `confirmed_findings`, `advisory_findings`, `observations`, `investigation_log`, `residual_risk`.

Rules:
- Do not invent new findings.
- Keep only findings already present in the merged input.
- Remove duplicates that describe the same root cause.
- Preserve the highest severity when duplicate findings disagree.
- Do not downgrade P1/P2 findings just because they are advisory or medium-confidence.
- Prefer precise, actionable titles and fixes.
- Preserve useful observations and residual risk items.
- If the merged input has no confirmed/advisory findings, return empty `confirmed_findings` and `advisory_findings` arrays.
