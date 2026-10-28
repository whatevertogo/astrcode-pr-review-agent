# Revision and protocol incident regression

The five failures since the 2026-10-22 deployment have three causes:

- #1299, #1300, #1319: checkout fetched `refs/remotes/origin/<base>`, but staging
  computed merge-base using the bare local branch name. Missing local branches
  failed; stale local branches could silently widen the review range. The CLI
  replay's `pin_base` created a local alias, masking the production failure.
- #1306: `gh pr diff --name-only` hit GitHub's 20,000-line diff limit (HTTP 406).
  File names must come from the paginated files API, not the full diff endpoint.
- #1308: the final tagged response contained literal `{}` in a source fact. The
  parser accepted that object as an empty defaulted review before reading tags.
  Receipt repair preserved the incorrectly empty original, discarding valid facts.

## Changes

Resolve an immutable base/head/merge-base snapshot from GitHub metadata and the
checked-out objects. Use its explicit SHA range for checks and all review stages;
do not create local branch aliases in replay. Verify paginated file count, unique
paths and the local changed-path set, including both paths of a rename. Check
revision identity after collection and before publication. An unrelated base-tip
advance is allowed if head, target branch and merge-base are unchanged.

Choose review JSON or tagged protocol deliberately. A JSON object must contain
review fields; code objects inside tagged evidence are not review envelopes.
Keep original evidence through receipt repair. Save complete chained failures
instead of losing the underlying GitHub/command error in state and comments.

This adapts PR-Agent's [revision/file collection checks and merge-base content](https://github.com/The-PR-Agent/pr-agent/blob/main/pr_agent/git_providers/github_provider.py).
Its [review prompt](https://github.com/The-PR-Agent/pr-agent/blob/main/pr_agent/settings/pr_reviewer_prompts.toml)
also separates visible evidence from assumptions about unseen code. Here, tools
remain available for related callers and failure paths; the four existing lenses
and P3 policy are unchanged. No new finding quota or blanket suppression is added.

## Verification

- 79 local tests passed; one opt-in real-host regression was run separately.
- The real-host test resolved #1300's original stacked base, collected and checked
  all 256 current #1306 files despite the full-diff 406, and parsed #1308's original
  final response with eight retained investigation entries and completion receipt.
- `cargo check` and the Linux musl release build passed.
- Artifact SHA256: `10830402aa962d748c989edd09326e38f37d200cf163e90b42fe8e458810b0f8`.

These are deterministic incident replays, not new precision/recall benchmarks.
The user authorized retrying the still-open #1300 and #1308 after deployment;
already-merged PRs are not requeued. Old authentication and environment failures
remain historical records, not evidence that all external failure modes are fixed.
