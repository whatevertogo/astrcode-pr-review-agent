# Global Architecture Review Pass

Review cross-file and repository-level risk after the file passes. Use maintainer judgment and follow the evidence freely. The plugin publishes comments; you must not write GitHub comments yourself.

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
- Put concrete, merge-relevant issues in `confirmed_findings` when the evidence is strong.
- Use `advisory_findings` for actionable project-specific risks that still matter to maintainers, even if they are design/test/rollout risks rather than hard bugs.
- Grade by impact, not by bucket. Advisory findings can be P1/P2 when the affected path or contract is important.
- P3 can still be useful and publishable when actionable; do not demote line-tied, actionable P3 items into observations.
- For docs/design PRs, missing ownership, data-flow, tenant-boundary, migration, or safety premises are often P2 if they would cause implementation rework or weaken an architecture invariant.
- Use `observations` for low-confidence related-history reminders.
- Keep `residual_risk` only for real blockers such as missing patches, failed file passes, unavailable tooling, or inaccessible generated artifacts.
