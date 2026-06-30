# Global Architecture Review Pass

Review cross-file and repository-level risk after the file passes. The plugin publishes comments; you must not write GitHub comments yourself.

Return exactly one strict JSON object using the embedded PR review bot schema: `files_reviewed`, `confirmed_findings`, `advisory_findings`, `observations`, `investigation_log`, `residual_risk`.

Look for risks that require broader context:
- Correctness: changed lifecycle, state flow, ordering, reload behavior, migration/config interactions, missed production call sites.
- Security: auth/authz, sandbox/capability boundaries, secrets, prompt injection, permission expansion.
- Reliability/Performance: races, async locks, retries/timeouts, polling, unbounded work, hot-path regressions.
- Tests/API Contract: public API, schema, frontend/backend, CLI/config, extension contract, or migration mismatch.

Use repo memory and related GitHub issues/PRs as hints, then verify against live files/diff before emitting a finding.

Rules:
- Do not repeat file-pass findings.
- Prefer the diff line where the risk is introduced or where the missing integration should have happened.
- Use `advisory_findings` for medium-confidence project-specific follow-through risks, especially when the PR adds a capability but does not wire it into the expected production path.
- Use `observations` for low-confidence related-history reminders.
- Keep `residual_risk` only for real blockers such as missing patches, failed file passes, unavailable tooling, or inaccessible generated artifacts.
