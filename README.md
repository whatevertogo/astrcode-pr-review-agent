# Astrcode PR Review Agent

Astrcode PR Review Agent is an external s5r extension for Astrcodey. It watches
GitHub pull requests, creates one persistent Astrcode review session per PR, and
publishes automated review results back to GitHub.

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
- Reuses one persistent Astrcode session per PR.
- Runs a coverage-first review pipeline inspired by PR-Agent style workflows:
  orientation, file shards, global risk pass, aggregation, and final summary.
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
  "protocol": { "s5r": "1.0" },
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
  "review_pipeline": "coverage_first",
  "max_inline_comments": 12,
  "inline_priority_max": "P2",
  "nitpick_inline_priority_max": "P3",
  "memory_dir": "~/.astrcode/pr-review-agent/memory",
  "worktree_dir": "~/.astrcode/pr-review-agent/worktrees"
}
```

The current production deployment can be made more verbose by setting P3 and
advisory limits higher:

```json
{
  "inline_priority_max": "P3",
  "max_inline_comments": 100,
  "max_advisory_inline_comments": 100,
  "max_p3_inline_comments": 100,
  "max_nitpick_inline_comments": 100
}
```

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

## Development

```bash
cargo fmt
cargo test
cargo check
```

The crate depends on `astrcode-extension-sdk` from the Astrcodey repository.
