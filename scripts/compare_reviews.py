#!/usr/bin/env python3
"""Run paired, sequential reviews against frozen PR snapshots without publishing."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time

SAMPLES = [("VitaDynamics/Vvbot", 1081), ("VitaDynamics/Vvbot", 1083),
           ("VitaDynamics/Vvbot", 968), ("whatevertogo/astrcodey", 49)]


def wait_for_idle():
    """Wait for the whole resident review, including gaps between model turns."""
    home = Path.home() / ".astrcode"
    deadline = time.monotonic() + 1800
    while True:
        state = json.loads((home / "pr-review-agent/state.json").read_text())
        active = set()
        for collection in ("auto_pr_reviews", "processed_comments"):
            for entry in state.get(collection, {}).values():
                if entry.get("status") == "running":
                    key = f"{entry['repo']}#{entry['pr_number']}"
                    active.add(key)
        if not active:
            return
        if time.monotonic() > deadline:
            raise RuntimeError("resident review still active; experiment did not interrupt it")
        print(f"waiting for {len(active)} resident review(s)", flush=True)
        time.sleep(15)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    args = parser.parse_args()
    root = args.root.resolve()
    root.mkdir(parents=True, exist_ok=True)
    binary = args.binary.resolve()
    manifest = {"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "runs": []}
    for index, (repo, number) in enumerate(SAMPLES):
        sample = root / f"{repo.replace('/', '__')}-{number}"
        snapshot = sample / "snapshot"
        common = [str(binary), "review", "--repo", repo, "--pr", str(number)]
        snapshot.mkdir(parents=True, exist_ok=True)
        with (snapshot / "prepare.log").open("a") as log:
            historical = ["--head", "50b8d4a6129ea13ae681fd606ac6c046891d6ea5"] if number == 968 else []
            prepared = subprocess.run(common + ["--output-dir", str(snapshot), "--prepare-only"] + historical, stdout=log, stderr=subprocess.STDOUT)
        if prepared.returncode:
            manifest["runs"].append({"repo": repo, "pr": number, "phase": "prepare", "exit_code": prepared.returncode})
            (root / "manifest.json").write_text(json.dumps(manifest, indent=2))
            continue
        order = ("baseline", "quality_first") if index % 2 == 0 else ("quality_first", "baseline")
        for pipeline in order:
            wait_for_idle()
            output = sample / pipeline
            output.mkdir(parents=True, exist_ok=True)
            print(f"START {repo}#{number} {pipeline}", flush=True)
            started = time.time()
            binary_sha = hashlib.sha256(binary.read_bytes()).hexdigest()
            with (output / "process.log").open("a") as log:
                process = subprocess.run(common + ["--output-dir", str(output), "--pipeline", pipeline,
                    "--snapshot", str(snapshot / "snapshot.json")], stdout=log, stderr=subprocess.STDOUT)
            record = {"repo": repo, "pr": number, "pipeline": pipeline,
                      "binary_sha256": binary_sha,
                      "exit_code": process.returncode, "started_at": started, "finished_at": time.time()}
            manifest["runs"].append(record)
            (root / "manifest.json").write_text(json.dumps(manifest, indent=2))
            print(f"END {repo}#{number} {pipeline} exit={process.returncode}", flush=True)


if __name__ == "__main__":
    main()
