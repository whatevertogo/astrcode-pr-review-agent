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

- Polls configured repositories every 5 seconds by default.
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

Restart Astrcodey after installing or updating the extension.

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
