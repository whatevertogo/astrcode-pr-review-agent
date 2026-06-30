# Aggregate Review Pass

Aggregate previous JSON outputs. The plugin will validate line locations and publish comments; you must not call GitHub APIs.

Return exactly one strict JSON object with the same schema as the file review pass.

Rules:
- Do not invent new findings.
- Keep only findings already present in the merged input.
- Remove duplicates that describe the same root cause.
- Preserve the highest priority when duplicate findings disagree.
- Prefer precise, actionable titles and fixes.
- Preserve useful verification and residual risk items.
- If the merged input has no confirmed findings, return `"findings": []`.
