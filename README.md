# Astrcode PR Review Agent

Astrcode PR Review Agent is an external s5r extension for Astrcodey. It watches
GitHub pull requests, runs staged Astrcode reviews, and publishes structured
review results back to GitHub. A standalone CLI can analyze a fixed PR without
the polling queue or GitHub writes.

The plugin is designed for the Astrcodey extension architecture: Astrcodey starts
the s5r worker from `extension.json`, while the worker performs lightweight
polling, queues PR review work, and uses Astrcode sessions for the actual
analysis.

## Features

- Runs the serial model queue every 5 seconds by default; independent discovery workers keep receiving comments during long reviews.
- Reacts to PR comments that mention the configured account, for example
  `@whatevertogo review it`.
- Automatically reviews newly discovered open PRs once, without replaying all
  existing PRs on first startup.
- Uses isolated sessions for review stages, sharing at most 8 KB of source-anchored contracts and prior candidates instead of investigation histories; ordinary PR conversations retain their persistent session.
- Defaults new configurations to a quality-first pipeline: file shards, candidate/global verification, and a deterministic report.
- Preserves `coverage_first` for existing configurations and paired baseline evaluation.
- Embeds review instructions from `prompts/` at compile time; no separate remote
  `reviewnow` skill directory is required.
- Validates findings against GitHub diff lines and posts inline review comments
  through GitHub's Pull Request Review API.
- Stores per-repository and per-PR review memory to reduce duplicate findings.

## Runtime Modes

```bash
astrcode-pr-review-agent s5r
astrcode-pr-review-agent poll
astrcode-pr-review-agent status
```

- `s5r` starts the Astrcode extension worker and background poll loop.
- `poll` runs one polling pass, useful for diagnostics.
- `status` prints the current configuration, queue state, failures, memory path,
  and recent run status.

## Install

Build the binary and copy it with `extension.json` into Astrcode's extension
directory:

```bash
cargo build --release
mkdir -p ~/.astrcode/extensions/astrcode-pr-review-agent
cp target/release/astrcode-pr-review-agent ~/.astrcode/extensions/astrcode-pr-review-agent/
cp extension.json ~/.astrcode/extensions/astrcode-pr-review-agent/
```

`extension.json` starts the worker as:

```json
{
  "extension_id": "astrcode-pr-review-agent",
  "protocol": { "s5r": "3.0" },
  "command": ["./astrcode-pr-review-agent", "s5r"]
}
```

Activate updates through the host extension reload endpoint (`POST /api/extensions/reload`) during an idle window. No host framework upgrade is required.

## Configuration

The plugin creates and reads:

```text
~/.astrcode/pr-review-agent/config.json
```

Important defaults:

```json
{
  "github_user": "whatevertogo",
  "repos": ["VitaDynamics/Vvbot", "whatevertogo/astrcodey"],
  "mention_repos": ["VitaDynamics/Vvbot", "whatevertogo/astrcodey"],
  "trusted_comment_authors": ["whatevertogo", "catDforD", "letr007", "united-pooh", "Soulter"],
  "mention": "@whatevertogo",
  "poll_interval_seconds": 5,
  "webhook_enabled": false,
  "auto_review_new_prs": true,
  "auto_review_bootstrap_existing_open_prs": false,
  "review_pipeline": "quality_first",
  "max_review_passes_per_pr": 16,
  "max_inline_comments": 12,
  "inline_priority_max": "P2",
  "nitpick_inline_priority_max": "P3",
  "memory_dir": "~/.astrcode/pr-review-agent/memory",
  "worktree_dir": "~/.astrcode/pr-review-agent/worktrees"
}
```

The quality-first pipeline publishes only confirmed, high-confidence P0–P2
findings. Advisory, lower-confidence, and overflow findings remain in the folded
summary with their evidence. `max_inline_comments` limits inline comments (zero
means no inline comments in this pipeline). Existing explicit pipeline choices
are not migrated automatically.

## Review prompts and comment presentation

Both pipelines compose the same small prompt blocks at compile time:

| Block | Responsibility |
|---|---|
| `review-method.md` | Behavior/causality, permissions, lifecycle/failure paths, and contracts/consumers; investigate only applicable questions, with no findings quota |
| `finding-evidence.md` | Trigger, introduced change, evidence, impact, counterexamples and a concrete action; distinguish absent evidence from proven absence |
| `comment-style.md` | Concise Chinese fields with visible conditions, impact, code evidence and a minimal fix direction |
| Stage and protocol files | Define each pass's task and retain the existing tagged-Markdown or JSON wire format |

Changing a shared block changes the composed prompt included in the review input
identity, so results from the old prompt are not reused. Model selection and
publication thresholds stay independent of review depth.

Both review pipelines require a final global pass, including single-shard and
zero-candidate reviews. The global pass returns the complete final result and an
explicit disposition for every prior finding/observation; omitted candidates,
invalid result references or a missing completion receipt fail validation. A
single bounded format repair is allowed. Failed global review never publishes
unreviewed findings as confirmed inline defects.

`max_review_passes_per_pr` includes the global pass (minimum 2); in coverage-first
mode it also includes orientation when the budget is at least 3. At budget 8 the
plan is orientation + up to 6 file passes + global, even when all 6 file passes
are used. Larger PRs need a larger budget; exhaustion remains explicitly partial.
Files start pending and only a successful file-stage declaration earns reviewed
coverage. Orientation/global reads never silently upgrade that coverage.

Coverage-first stage caches bind base/head, configuration, model identity, prompt
and input context. Old unbound records remain on disk but are not reused. Global
candidate dispositions and file coverage are persisted and the program adds a
review receipt to the GitHub overview.

Inline comments show the issue, impact, evidence and suggested action directly;
only supplementary context is folded. Unconfirmed advice is visibly marked.
The final-report prompt avoids mandatory merge verdicts and repeated sections.
The fallback report preserves saved evidence and missing-validation warnings;
publication receipts identify which findings actually became inline comments.
Session/trigger metadata is available in a fold below the result.

Re-render a saved result without running a model or contacting GitHub:

```bash
cargo run --example render_review -- path/to/result.json > preview.md
```

Presentation uses GitHub-supported Markdown: block evidence starts separately from
its label, populated detail groups include counts, and separate findings retain
their file/line and have visual dividers. Plain titles and table cells are escaped;
code paths with backticks use a matching longer delimiter. Usage tables use four
columns with line breaks inside cells so narrower comment panes stay readable.
No custom CSS or external badges are emitted in GitHub comments.

This changes presentation only; it does not repair or revalidate historical findings.

## Reliable comment receipt

Only open PR conversation comments are eligible. Every entry point checks the
canonical GitHub comment author, exact `@whatevertogo` mention, PR ownership and
open state. Trusted authors may trigger in any accessible repository; all authors
may trigger in `mention_repos`. Missing/null `mention_repos` inherits `repos`, while
an explicit empty list grants no repository-wide comment permission. AstrBot is
not a priority repository. New-PR automatic reviews still use only `repos`.
Agent-marked replies are ignored; a human mentioning their own account is allowed.

Independent discovery sources use no model calls:

- Priority repositories: direct comment reads every 60 seconds, complete pagination
  ordered by update time and a 10-minute overlap. The cursor advances only after
  successful receipt of all candidates.
- Trusted users: all available activity pages every 60 seconds, or the longer
  `X-Poll-Interval`, with per-page ETags (conditional response caching). Event order
  is not assumed. [GitHub activity](https://docs.github.com/en/rest/activity/events)
  can lag 30 seconds to 6 hours and exposes only 30 days / 300 events; other users'
  private activity can be invisible. Window gaps and API errors appear in status.
- Global mention search: paginated every 120 seconds, with visible warnings for
  incomplete results or GitHub's 1,000-result ceiling. The legacy
  `mention_search_limit` field remains loadable but no longer truncates this scan.

Each new source initially looks back 24 hours. Search retains this initial cutoff
so delayed indexing can still recover older candidates. Sources have separate
locks, checkpoints and backoff; a broken search cannot block direct receipt.

```bash
astrcode-pr-review-agent enqueue --comment-url 'https://github.com/OWNER/REPO/pull/123#issuecomment-456' --dry-run
astrcode-pr-review-agent enqueue --comment-url 'https://github.com/OWNER/REPO/pull/123#issuecomment-456'
astrcode-pr-review-agent status --comment-url 'https://github.com/OWNER/REPO/pull/123#issuecomment-456'
astrcode-pr-review-agent status
```

The link bypasses discovery indexes, never authorization. `--dry-run` does not
write files, react, or invoke a model. Receipt returns `queued`, `already_pending`,
`running`, `already_processed`, or a rejection/failure reason. Ordinary requests
such as explaining test failures retain their original text and conversation route.
The eyes reaction means **durably received**, not model started; a reaction failure
does not cancel or repeat the task. A processed comment is never rerun on edit.

`mention-inbox/` contains immutable atomic receipts. Only the executor writes
`state.json`, saving a pending task before deleting its receipt. Pending tasks are
consumed directly after restart and revalidated before execution; deleted comments,
removed mentions, revoked permission and closed PRs are recorded as aborted.
`mention-retries/` records validation backoff; `mention-discovery/` records per-source
success, errors, polling deadlines and upstream limitations. Corrupt review state
stops processing instead of silently resetting the deduplication ledger.

Webhook delivery envelopes are atomically saved in `webhook-inbox/`; the same live
comment checks apply across repositories. Legacy JSONL spools, including previously
claimed files, are migrated without discarding pending records. `discover` runs one
due discovery pass for diagnostics; `poll` performs discovery and one executor pass.

Before deployment, back up the binary, configuration and persistent data, check for
active tasks, then atomically replace the binary and reload extensions. Preserve
`coverage_first` and current host/model settings when deploying only trigger fixes.
For rollback, stop new discovery by unloading this worker, restore the old binary
and configuration, and reload. **Keep the newest state, inboxes and reply receipts**;
never restore the old state snapshot. The old worker cannot consume new inbox files:
retain them for replay by the fixed worker, and check status before manual recovery.

## Isolated single-run review

Run on the same machine as AstrCodey, using its existing model configuration:

```bash
astrcode-pr-review-agent review --repo OWNER/REPO --pr 123 --output-dir /absolute/path/run
```

This reads the installed configuration, but keeps checkout caches, stage results,
and review-agent state under the output directory. It does not start a poller,
restart AstrCodey, or publish to GitHub. A run directory is locked against concurrent
use. Model settings, base/head revisions, applicable instructions and prompts are
part of the cache identity; changed inputs invalidate stage reuse.

- `--prepare-only` freezes a `snapshot.json` including immutable base/head, code
  patches, path instructions and deterministic check receipts.
- `--snapshot PATH` reviews that frozen input, including closed historical PRs. Publication still requires the current base/head to match exactly; merged or closed PRs receive an explicit lifecycle note.
- `--head FULL_SHA` prepares a historical PR commit, deriving its merge base and
  immutable file manifest. The SHA must belong to that PR. Current descriptions
  are omitted because they may reveal later fixes. Historical results still
  cannot publish unless they match the current PR base/head.
- `--pipeline baseline` evaluates the original coverage-first analysis and final
  model report without publishing. Existing review answers are excluded from the
  collected snapshot; audit tool logs for accidental answer access during evaluation.
- `--publish` explicitly publishes the result to the requested PR. Both base
  and head must still match. Reusing the same run directory replays successful
  stages and reconciles GitHub markers before posting missing comments.

Artifacts are `result.json` (`ReviewRunResult`, schema version 3), `report.md`,
`stages.json`, prompts/responses, and archived session logs. Each stage receipt
includes its session, input identity, completion/failure and actual usage.
Failed/retried attempts remain accounted for. Provider usage, estimates, missing
usage, and unknown cache accounting are distinguished; reasoning output and cache
subsets are never added to totals twice. Dollar cost is not inferred from token
counts alone.

Older result records are revalidated from compatible cached stage outputs; their
rendered/placement decisions are not treated as current validation. Saved parse
failures can be recovered after a decoder update without another model request,
preserving their original usage and error. Original priorities and full evidence
survive summary-only placement. Global verification reconciles all shard
observations, including contradictory or superseded claims, before publication.

Large patches are split at hunk boundaries. Single hunks larger than the configured
budget, absent textual diffs, exhausted pass budgets and failed checks are reported
as incomplete. A global verification pass is reserved for multi-shard reviews or
candidate findings. Missing inline anchors never get moved to nearby lines.

Publication uses one owned summary comment plus a batch COMMENT review. It never
approves or requests changes. After ambiguous POST failures, GitHub is queried for
owned finding markers before an explicit retry. Local artifacts preserve the full
result even if a GitHub payload is too large or publication fails. Prompts direct
model tools to read-only investigation; this is not an OS sandbox.

For the fixed four-sample paired campaign:

```bash
python3 scripts/compare_reviews.py --binary /absolute/path/astrcode-pr-review-agent --root /absolute/path/evaluation
```

The campaign runs sequentially, alternates baseline/optimized order, and waits for
resident review sessions to become idle. It never publishes. Review findings must
be adjudicated against source before interpreting counts as precision or recall.

## Requirements

- Astrcodey with s5r extension support.
- `gh` authenticated as the GitHub account that should publish reviews.
- Network access to GitHub and to the local Astrcode server.
- A working Rust toolchain for building from source.

## Memory Layout

```text
~/.astrcode/pr-review-agent/
  config.json
  state.json
  run.lock
  memory/
    runs.jsonl
    repos/
      owner__repo/
        index.md
        pr-123.md
  worktrees/
```

Memory records session IDs, reviewed ranges, posted finding fingerprints,
summary observations, and final review URLs.

Repository checkout keeps one shared bare repo cache per configured GitHub repo,
fetches only the PR and its base branch, then creates lightweight PR worktrees
from that cache. Checkout commands retry transient failures up to three times
and have a 600-second timeout by default. Set
`ASTRCODE_PR_REVIEW_AGENT_CHECKOUT_COMMAND_TIMEOUT_SECONDS` to at least 30 to
override that timeout for unusual networks.

Deterministic Rust verification targets only crates containing changed Rust
files by default. Cargo manifest/lock/toolchain changes, changes spanning more
than four crates, or an explicit full-test trigger expand verification to the workspace.
The review passes still inspect cross-crate consumers independently of this
build scope; a passing targeted check is evidence, not proof that the whole PR
is correct.

## Development

```bash
cargo fmt
cargo test
cargo check
```

The crate depends on `astrcode-extension-sdk` from the Astrcodey repository.
