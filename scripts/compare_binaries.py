#!/usr/bin/env python3
"""Replay installed/candidate coverage-first binaries against identical snapshots.

No publication, deployment, policy changes or automated quality verdict. The resident
executor lock is held while running; discovery may still queue incoming work.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--resume", action="store_true")
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--sample", action="append",
                        help="OWNER/REPO#NUMBER@HEAD_SHA; repeat for each sample")
    source.add_argument("--cases", type=Path, help="Pinned case file; expectations stay out of model prompts")
    args = parser.parse_args()
    samples = args.sample
    if args.cases:
        cases = json.loads(args.cases.read_text())["cases"]
        samples = [f"{case['repo']}#{case['pr']}@{case['head']}" for case in cases]
    if len(samples) != len(set(samples)):
        raise ValueError("duplicate frozen samples")
    root = args.root.resolve()
    root.mkdir(parents=True, exist_ok=args.resume)
    resident = Path.home() / ".astrcode/pr-review-agent"
    git_config = Path.home() / ".gitconfig"
    original_git_config = git_config.read_bytes() if git_config.exists() else None
    test_rewrite = b"x-access-token:test-token@github.com"
    if original_git_config and test_rewrite in original_git_config:
        raise RuntimeError("host has an invalid test-token Git rewrite; do not start comparison")
    policy = json.loads((resident / "config.json").read_text())
    # Do not run repository-wide deterministic builds while preparing snapshots.
    # Model tool calls still need separate auditing; this is not an execution sandbox.
    policy["deterministic_checks_enabled"] = False
    policy_path = root / "config.json"
    if policy_path.exists() and json.loads(policy_path.read_text()) != policy:
        raise RuntimeError("installed policy changed; use a fresh comparison directory")
    policy_path.write_text(json.dumps(policy, indent=2))
    env = dict(os.environ, ASTRCODE_PR_REVIEW_AGENT_DATA_DIR=str(root))
    binaries = {"baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
    hashes = {k: digest(v) for k, v in binaries.items()}
    manifest = {"binary_sha256": hashes,
                "checks": "disabled for both historical replays", "runs": [],
                "quality_gate": "manual source adjudication required; never infer from counts"}
    if args.resume:
        manifest = json.loads((root / "manifest.json").read_text())
        for record in manifest["runs"]:
            variant = record.get("variant")
            if variant:
                record.setdefault("binary_sha256", manifest["binary_sha256"][variant])
        manifest["binary_sha256"] = hashes

    def save():
        (root / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2))

    def run(command, output):
        output.mkdir(parents=True, exist_ok=True)
        with (output / "process.log").open("a") as log:
            return subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT).returncode

    with (resident / "run.lock").open("a") as lock:
        # Never interrupt a review already in progress.
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        save()
        for index, sample in enumerate(samples):
            identity, head = sample.rsplit("@", 1)
            repo, number = identity.rsplit("#", 1)
            directory = root / f"{repo.replace('/', '__')}-{number}-{head[:12]}"
            snapshot = directory / "snapshot"
            common = ["review", "--repo", repo, "--pr", number]
            code = run([str(binaries["baseline"]), *common, "--head", head,
                        "--output-dir", str(snapshot), "--prepare-only"], snapshot)
            if code:
                manifest["runs"].append({"sample": sample, "phase": "prepare", "exit_code": code})
                save()
                raise RuntimeError(f"snapshot failed: {snapshot / 'process.log'}")
            snapshot_path = snapshot / "snapshot.json"
            snapshot_sha256 = digest(snapshot_path)
            order = ("baseline", "candidate") if index % 2 == 0 else ("candidate", "baseline")
            pair = []
            for variant in order:
                previous = next((r for r in reversed(manifest["runs"])
                                 if r.get("sample") == sample and r.get("variant") == variant
                                 and r.get("binary_sha256") == hashes[variant]
                                 and r.get("snapshot_path") == str(snapshot_path)
                                 and r.get("snapshot_sha256") == snapshot_sha256
                                 and r.get("exit_code") == 0 and r.get("status") == "complete"
                                 and r.get("tracked_worktree_clean") is True
                                 and r.get("worktree_head") == head
                                 and not r.get("host_git_config_changed")), None)
                if previous:
                    saved = Path(previous.get("output_dir", directory / variant)) / "result.json"
                    if saved.exists():
                        pair.append(previous)
                        print(f"REUSE {identity} {variant}", flush=True)
                        continue
                output = directory / (f"{variant}-{hashes[variant][:12]}" if args.resume else variant)
                print(f"START {identity} {variant}", flush=True)
                started = time.time()
                code = run([str(binaries[variant]), *common, "--pipeline", "baseline",
                            "--snapshot", str(snapshot_path),
                            "--output-dir", str(output)], output)
                record = {"sample": sample, "variant": variant, "exit_code": code,
                          "started_at": started, "finished_at": time.time(),
                          "binary_sha256": digest(binaries[variant]), "output_dir": str(output),
                          "snapshot_path": str(snapshot_path), "snapshot_sha256": snapshot_sha256}
                worktree = output / "worktrees" / repo.replace("/", "__") / f"pr-{number}"
                if worktree.exists():
                    actual = subprocess.run(["git", "rev-parse", "HEAD"], cwd=worktree,
                                            capture_output=True, text=True, check=True)
                    record["worktree_head"] = actual.stdout.strip()
                    record["tracked_worktree_clean"] = subprocess.run(
                        ["git", "diff", "--quiet", "HEAD", "--"], cwd=worktree).returncode == 0
                current_git_config = git_config.read_bytes() if git_config.exists() else None
                if current_git_config != original_git_config:
                    record["host_git_config_changed"] = True
                    # Remove only the known accidental test rule, never restore over unrelated edits.
                    if current_git_config and test_rewrite in current_git_config:
                        subprocess.run(["git", "config", "--global", "--unset-all",
                                        "url.https://x-access-token:test-token@github.com/.insteadOf"],
                                       check=True)
                    restored = git_config.read_bytes() if git_config.exists() else None
                    record["host_git_config_restored"] = restored == original_git_config
                if record["binary_sha256"] != hashes[variant]:
                    raise RuntimeError("binary changed during comparison")
                result = output / "result.json"
                if result.exists():
                    data = json.loads(result.read_text())
                    record.update({"model": data["model"], "base_sha": data["base_sha"],
                                   "head_sha": data["head_sha"], "status": data["status"]})
                manifest["runs"].append(record)
                pair.append(record)
                save()
                print(f"END {identity} {variant}: {code}", flush=True)
                if digest(snapshot_path) != snapshot_sha256:
                    raise RuntimeError(f"frozen input snapshot changed; deployment gate closed: {output}")
                if code:
                    raise RuntimeError(f"review failed; deployment gate closed: {output}")
                if record.get("host_git_config_changed"):
                    raise RuntimeError(f"host Git configuration changed; deployment gate closed: {output}")
                if record.get("worktree_head") != head or not record.get("tracked_worktree_clean"):
                    raise RuntimeError(f"frozen worktree changed; deployment gate closed: {output}")
            for key in ("model", "base_sha", "head_sha", "snapshot_sha256"):
                if pair[0].get(key) != pair[1].get(key):
                    raise RuntimeError(f"non-comparable pair: {key}")


if __name__ == "__main__":
    main()
