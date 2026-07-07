# Aggregate Review Pass

Aggregate previous pass outputs. The plugin will validate line locations and publish comments; you must not call GitHub APIs.

Prefer the embedded tagged Markdown protocol. Strict JSON is accepted, but do not let schema formatting make you downgrade or erase real findings.

Rules:
- Do not invent new findings.
- Keep only findings already present in the merged input.
- Remove duplicates that describe the same root cause.
- Preserve the highest severity when duplicate findings disagree.
- Do not downgrade P1/P2 findings just because they are advisory or medium-confidence.
- Prefer precise, actionable titles and fixes.
- Preserve useful observations and residual risk items.
- If the merged input has no confirmed/advisory findings, return empty `confirmed_findings` and `advisory_findings` arrays.
