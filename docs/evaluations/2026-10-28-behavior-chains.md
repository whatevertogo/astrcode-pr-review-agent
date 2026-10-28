# Behavior-chain review: implementation and evaluation boundary

## Scope

This change implements the first two priorities from the design discussion:
behavior-oriented task organization and a reproducible quality comparison. It
does not add independent verifier sessions, feedback-memory ingestion, more
model rounds, a new system prompt, or a different P3 publication policy.

Previously the plugin packed all shards before orientation ran. Orientation could
describe cross-file behavior, but its discoveries could not influence assignment.
Now orientation optionally emits `review_chain` JSON blocks (or `review_chains`
in legacy JSON output), which describe changed paths, related paths and checks.
The deterministic planner drops unusable changed/related paths individually,
ignores chains left without an eligible changed path, merges overlapping groups
and reserves their prompt bytes before packing. Unassigned files are never
omitted. If hints are malformed, too large, or worsen pass-budget coverage, the
ordinary planner remains authoritative. Related paths are investigation leads,
not reviewed files.
Budget comparison preserves every path the ordinary plan can fully review within
the available passes, including all parts of split files; equal shard counts alone
do not guarantee equal coverage after reordering.

The earlier A/B experiment showed a real regression when all migration code and
docs were merged into one long prompt. Consequently large mixed behavior groups
retain the existing docs-consumer split. Their common chain is carried into each
part, preserving cross-file questions without removing dedicated docs attention.

## References and adaptations

- [PR-Agent diff processing](https://github.com/The-PR-Agent/pr-agent/blob/main/pr_agent/algo/pr_processing.py): keep context within request budgets and explicitly track incomplete coverage. Our grouping hints do not replace coverage accounting.
- [Anthropic code review](https://github.com/anthropics/claude-plugins-public/blob/main/plugins/code-review/commands/code-review.md): separate investigation from validation and keep distinct issues actionable. We do not copy its diff-only, high-severity-only exclusions; conditional defects and P3 remain eligible.

## Quality comparison

`evaluations/cases.json` starts with three fixed revisions: #1269 before its lock
fix, #1269 after the lock-only fix, and #1271's intentionally ignored red test.
The lock fix was inspected through the commit API: commit
`96926d38088b9de4b0d163d869d17ecf2634a59f` modifies only `Cargo.lock`.
Ground truth is kept outside model prompts. The comparison runner stores different
heads in different directories and refuses to reuse runs with tracked-file or
host-Git side effects.

The offline evaluator hides variant labels, requires a source-backed disposition
for every finding, verifies matching snapshots/models, and leaves unjudged or
partial runs incomplete. It tests whether an issue is found before its fix and
not reported afterward, rather than rewarding comment counts. P3 and unplaced
findings are included; observations remain separate from confirmed detections.
Snapshot identity uses the original frozen input path and its pre-run digest,
not per-binary reserialized outputs whose default fields can differ. The runner
rejects input mutation, and the evaluator checks the recorded input again.

## Verification

- Rust: 80 tests passed; the previously documented real-host incident test is
  opt-in and is not a new model-quality experiment.
- Python: varied evaluator regression passed (blinding, frozen identity,
  unjudged/partial rejection, evidence requirement, fixed-before/after check).
- `cargo check` and Linux musl release compilation passed.
- No new behavior-chain model A/B result is claimed in this PR. The production
  worker was still reviewing #1300 during implementation; it was not interrupted
  or replaced to run experiments. This change is not deployed.

Before rollout, run both binaries against the frozen corpus, independently
adjudicate the findings, investigate any lost known issue or added false positive,
and expand the corpus beyond this starter set. Passing plumbing tests alone is
not evidence of improved review precision or recall.
