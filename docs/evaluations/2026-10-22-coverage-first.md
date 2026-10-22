# Coverage-first review comparison

## Scope

Improve the deployed coverage-first path without changing discovery, the four
review lenses, persistent PR sessions, model selection, or P3 publication policy.
The alternative quality-first path is not enabled by this change.

## Reproduction

`scripts/compare_binaries.py` accepts separate installed and candidate binaries.
It freezes each PR once, uses the same installed publication policy for both,
creates private sessions and worktrees, and never passes `--publish`. It holds the
resident executor lock to avoid model contention without stopping event discovery.
Runs record binary hashes, snapshot hashes, model identity and complete responses.
There is no automatic deploy step.

The plugin-wide deterministic checks are disabled for both historical variants.
Model tool calls must be audited separately; the runner is not a shell sandbox.
Production memory is deliberately not injected into historical model runs, to
avoid leaking previous findings. Memory selection is tested separately; these
paired runs do not establish the impact of memory changes on model accuracy.

Samples:

| PR | Frozen head | Purpose |
|---|---|---|
| VitaDynamics/Vvbot#1271 | `c4c547a53c6f2c590be1818fa528e8ec21545996` | Single-file red-test change; do not misreport the intentionally ignored failing test as a regression |
| VitaDynamics/Vvbot#1269 | `4c97cf80c1e1d63b31d2d36e245137aaf529a077` | Cross-file registry migration; retain source-backed manifest/lock and CI connectivity findings |

## Acceptance Criteria

- Same base/head, frozen source context, model/config identity and severity policy.
- Successful required file/global passes; no loss of previously covered changes.
- Recheck each material finding against the frozen source, not the old bot report.
- Do not treat the current private registry's contents as proof of historical availability.
- Distinguish scheduled/manual cache jobs from PR CI when describing impact.
- P3 remains eligible for inline publication under the installed policy.
- No duplicate root causes, fabricated test success or claims that unseen code was reviewed.
- No GitHub writes during comparison; no deployment on an incomplete/failed comparison.

Finding counts are not precision/recall. Two PRs are a smoke comparison, not a
general statistical guarantee of better review quality.

## Local Verification

- Baseline: 74 library tests passed before edits.
- Candidate: 76 tests passed, including complete-small-PR packing, multi-hunk and
  over-budget-hunk splitting, no omitted annotated lines, all-parts coverage,
  relevant-memory selection, P3 validation, four-lens presence, and whitespace/
  long-line preservation.
- `cargo check` and Linux musl release cross-build passed.

## Paired Results

The first candidate was **not approved for deployment**. Both samples completed
with identical code snapshots/model identity and full coverage (1/1 and 32/32).
It retained the Cargo.lock defect and removed the single-file run's future-fix
observation, but missed two actionable P3 documentation inconsistencies found by
the installed baseline. This is a quality regression despite fewer tokens/rounds.

| Initial run | Model requests | Tool calls | Input tokens (including cached) | Seconds |
|---|---:|---:|---:|---:|
| #1271 installed baseline | 45 | 53 | 1,600,883 | 563 |
| #1271 first candidate | 26 | 31 | 1,268,580 | 362 |
| #1269 installed baseline | 39 | 37 | 3,721,588 | 954 |
| #1269 first candidate | 39 | 36 | 1,819,423 | 572 |

The final prompt revision treats operational docs, skills and configuration examples
as consumers within the existing Tests/API Contract lens. It asks for targeted
checks of obsolete commands/prerequisites during migrations, not a documentation
finding quota. Final-binary replays reuse the completed baseline records, and run
in new private output directories keyed by the candidate binary hash.

During the installed baseline replay of #1269, an old contract test wrote a
`test-token` URL rewrite into the service account's global Git configuration.
Only that erroneous rule was removed. The resulting configuration matched the
pre-experiment backup byte-for-byte. No baseline replay is repeated, and model
tools/host configuration continue to be audited. Disabling the deterministic-check
pass did **not** prevent a model from running a repository test; it is not a sandbox.

The prompt-only revision (`a125efe5a7a6260a1651842d11d06bc4ce4186c30d2cf2dfb0a78b08101c55ac`)
restored the obsolete private-Git-dependency finding, but still missed the outdated
`flatc` prerequisites/troubleshooting. It was also **not approved for deployment**.
This is evidence against treating token fit alone as sufficient review granularity.

The next revision keeps small mixed PRs and same-kind PRs together, but separates
code/configuration and documentation/skills for larger mixed PRs before byte packing.
`max_files_per_shard` is used only as the small-mixed-PR threshold, not a hard group
size. The existing four lenses apply to both groups; this is not a fifth lens or
a mandatory P3 quota. All candidates still go through the final global adjudicator.

The intermediate cohort artifact `c091e4c71aef59df670545bd127d7de4d926713ff15741314c5b0e3fbecd55e6`
was retired during evaluation after code review found that instruction space was
reserved twice. The final implementation renders empty instruction framing and
reserves the instruction allowance once. A regression assertion checks that adding
instructions increases the prompt by exactly their byte length. Hunk detection
also requires a line-start header, so `@@` in a filename cannot corrupt splitting.

## Final Build Results

Artifact: `a592ce19a2f32ee5936be2424e8b89b2e5dd297ff48271b8864d76d19694fda9`.

| Sample | Installed baseline | Final build |
|---|---|---|
| #1271 | 1/1 reviewed; no findings; one future-fix observation | 1/1 reviewed; no findings or observations |
| #1269 | 32/32 reviewed; one P1 and two P3 findings | 32/32 reviewed; same P1 and both P3 findings retained by global adjudication |

#1269 now uses three file passes: 20 code/configuration files, two remaining
code/test files, and ten documentation/skill files, rather than eight arbitrary
four-file passes. The final global result retains both the obsolete Git-dependency/
flatc troubleshooting issue and the stale board-deploy skill prerequisite.

The final validator routes the two P3 findings to `unplaced_findings` because their
actual source anchors are not commentable diff lines. They are not filtered by
severity. The saved model report includes their complete explanation and fixes;
it does not invent nearby inline anchors. No test review/comment was published.

| Final comparison | Model requests | Tool calls | Input tokens (including cached) | Seconds |
|---|---:|---:|---:|---:|
| #1271 installed baseline | 45 | 53 | 1,600,883 | 563 |
| #1271 final build | 31 | 36 | 1,284,697 | 397 |
| #1269 installed baseline | 39 | 37 | 3,721,588 | 954 |
| #1269 final build | 35 | 38 | 2,686,303 | 1,225 |

Fewer shards did not make every run faster. These are measured smoke results,
not a claim of statistically improved precision/recall or general latency.

## Initial Deployment Gate: Closed

The final candidate also ran the historical `board_build_fallback_contract.sh`,
which wrote the known `test-token` rewrite into the host's global Git configuration.
The erroneous rule was removed immediately, and the configuration matched the
pre-evaluation backup byte-for-byte. An abort was requested; the CLI subsequently
persisted a complete result. A complete model result is **not** acceptance when a
host-side effect has been observed. The explicit `host-side-effect-release.json`
record on the server keeps deployment unapproved independently of CLI exit status.

Manual post-run checks confirmed frozen HEAD and no tracked worktree changes for
both baseline and final samples. The installed production binary still matches
the baseline hash, the extension is enabled/loaded, the executor lock is free,
and the evaluation processes have ended. Production was not upgraded or restarted.

One remaining report-quality limitation: the final #1269 observation recommends
checking the cache-warming job in PR CI, although that job runs only on schedule
or manual dispatch. That validation advice must be narrowed; the underlying
network question is not a confirmed defect.

Before deployment, agree on tool-execution isolation. Astrcode exposes a
`ToolInputTransform` hook with session/worktree/tool context, but that is an
integration point, not a sandbox. A PR-scoped Git-configuration isolation guard
could address this accidental global write without replacing the default system
prompt; its compatibility and real-host behavior still need testing. Full sandboxing
and credential/network isolation are a separate, larger boundary.

## Follow-up: PR Git Configuration Guard

The user approved scoped Git configuration isolation and subsequent deployment.
The worker now declares `ToolIntercept` and registers the existing s5r blocking
tool-input transform. Only shell/bash calls in sessions under the configured PR
worktree root are wrapped. Each call includes the original global settings in a
fresh temporary config and redirects ordinary global writes there. Normal sessions
are unchanged. Explicit file writes, environment overrides and hostile commands
are not isolated; this is not a sandbox. Explicit global reads need `--includes`
to see inherited settings.

77 local tests and `cargo check` passed; the Linux musl build succeeded. A separate
real Astrcode server on the remote host loaded the candidate with polling disabled.
Its default session executed the historical board fallback contract through the
hook: exit code 0, `board_build_fallback_contract: ok`. Both the host config and a
staging global-config sentinel remained byte-identical. The temporary server was
stopped afterward; no GitHub comments were posted.

Validated artifact: `3e9983436c4300be7e426780709959e182cd3615a40387a278569e8febf2a85d`.
Session: `dd4a95eb-a667-4a74-9a1b-e93360e0a78f`.
The report prompt now explicitly checks workflow events/job conditions before
recommending PR CI validation. The earlier paired model runs remain the quality
evidence; this follow-up is a real hook/contract smoke test, not a new full A/B run
or a guarantee that all future model advice is correct.
