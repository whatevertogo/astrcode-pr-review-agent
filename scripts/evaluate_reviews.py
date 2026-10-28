#!/usr/bin/env python3
"""Blind paired review outputs, then score explicit source-backed human judgments.

This never calls a model or GitHub. Finding counts and keyword matches are not
quality labels. Keep key.json away from the reviewer until adjudication is done.
"""
import argparse
import hashlib
import json
from pathlib import Path
import random


def read(path):
    return json.loads(Path(path).read_text())


def prepare(cases, manifest, output, seed=0):
    output.mkdir(parents=True, exist_ok=False)
    packets, key, missing = [], {}, []
    identities = {}
    for case in cases["cases"]:
        sample = f"{case['repo']}#{case['pr']}@{case['head']}"
        pair = []
        for variant in ("baseline", "candidate"):
            record = next((r for r in reversed(manifest["runs"])
                           if r.get("sample") == sample and r.get("variant") == variant
                           and r.get("binary_sha256") == manifest["binary_sha256"][variant]
                           and r.get("status") == "complete" and r.get("exit_code") == 0), None)
            if record is None:
                missing.append(f"{case['id']}:{variant}")
                continue
            directory = Path(record["output_dir"])
            result = read(directory / "result.json")
            if (result.get("status") != "complete" or record.get("tracked_worktree_clean") is not True
                    or record.get("worktree_head") != case["head"] or record.get("host_git_config_changed")):
                raise ValueError("incomplete or side-effecting run is not quality evidence")
            # Each binary may reserialize the snapshot with different schema defaults.
            # Compare the frozen input passed to both runs, not those output copies.
            snapshot_digest = hashlib.sha256(Path(record["snapshot_path"]).read_bytes()).hexdigest()
            if result["head_sha"] != case["head"] or result["repo"] != case["repo"] or result["pr_number"] != case["pr"]:
                raise ValueError("result is not the frozen case")
            if snapshot_digest != record["snapshot_sha256"]:
                raise ValueError("snapshot changed after the run")
            identity = {k: result[k] for k in ("repo", "pr_number", "base_sha", "head_sha", "model")}
            identity["snapshot_sha256"] = snapshot_digest
            if case["id"] in identities and identities[case["id"]] != identity:
                raise ValueError("cannot compare different snapshots or models")
            identities[case["id"]] = identity
            review = result["review"]
            findings = []
            for location in ("inline_findings", "summary_findings", "unplaced_findings"):
                for finding in review.get(location, []):
                    findings.append({"id": f"F{len(findings) + 1:03}", "location": location, "finding": finding})
            report_path = directory / "baseline-model-report.md"
            pair.append((variant, {"repo": case["repo"], "pr": case["pr"],
                "head": case["head"], "findings": findings, "observations": review.get("observations", []),
                "investigation_log": review.get("investigation_log", []),
                "report": report_path.read_text() if report_path.exists() else "",
                "coverage": review.get("coverage"), "residual_risk": review.get("residual_risk", [])}))
        random.Random(f"{seed}:{case['id']}").shuffle(pair)
        for variant, packet in pair:
            packet_id = f"R{len(packets) + 1:03}"
            packet["id"] = packet_id
            packets.append(packet)
            key[packet_id] = {"case": case["id"], "variant": variant}
    for case in cases["cases"]:
        before = identities.get(case.get("paired_with"))
        after = identities.get(case["id"])
        if before and after and before["model"] != after["model"]:
            raise ValueError("cannot compare before/after fixes across different models")
    (output / "packets.json").write_text(json.dumps(packets, ensure_ascii=False, indent=2))
    (output / "key.json").write_text(json.dumps({"mapping": key, "missing": missing}, indent=2))
    judgments = {p["id"]: {"coverage_checked": False, "coverage_evidence": "", "findings": [
        {"id": f["id"], "verdict": "unjudged", "issue_ids": [], "evidence": ""} for f in p["findings"]]} for p in packets}
    (output / "judgments.json").write_text(json.dumps(judgments, indent=2))
    return {"packets": len(packets), "missing": missing}


def score(cases, packets, key, judgments):
    definitions = {c["id"]: c for c in cases["cases"]}
    rows = []
    for packet in packets:
        identity = key["mapping"][packet["id"]]
        case = definitions[identity["case"]]
        judgment = judgments.get(packet["id"], {})
        entries = judgment.get("findings", [])
        ids = [j["id"] for j in entries]
        expected_ids = {f["id"] for f in packet["findings"]}
        if len(ids) != len(set(ids)) or set(ids) != expected_ids:
            raise ValueError("every finding needs exactly one judgment")
        expected = {i["id"] for i in case["expected"]}
        forbidden = {i["id"] for i in case["forbidden"]}
        counts = {v: 0 for v in ("true_positive", "false_positive", "duplicate", "unresolved", "unjudged")}
        matched, forbidden_reported = set(), set()
        for j in entries:
            verdict = j["verdict"]
            if verdict not in counts:
                raise ValueError("unknown verdict")
            if verdict != "unjudged" and not j.get("evidence", "").strip():
                raise ValueError("a judgment needs source/test evidence")
            labels = set(j.get("issue_ids", []))
            if labels - expected - forbidden:
                raise ValueError("unknown issue label; update the corpus after independent validation")
            if verdict == "true_positive" and labels & forbidden:
                raise ValueError("a forbidden issue cannot count as a true positive")
            if verdict == "true_positive":
                if not labels & expected:
                    raise ValueError("a true positive needs a validated expected issue label; update the corpus first")
                matched.update(labels & expected)
            forbidden_reported.update(labels & forbidden)
            counts[verdict] += 1
        coverage = (packet.get("coverage") or {}).get("entries", {})
        complete_coverage = bool(coverage) and all(e["status"] in ("Reviewed", "SkippedGenerated") for e in coverage.values())
        complete = (not counts["unjudged"] and not counts["unresolved"] and complete_coverage
                    and judgment.get("coverage_checked") is True and bool(judgment.get("coverage_evidence", "").strip()))
        denominator = counts["true_positive"] + counts["false_positive"]
        rows.append({**identity, **counts, "expected_found": sorted(matched),
                     "expected_missed": sorted(expected - matched), "forbidden_reported": sorted(forbidden_reported),
                     "precision_on_judged_findings": counts["true_positive"] / denominator if denominator else None,
                     "known_issue_recall": len(matched) / len(expected) if expected else None,
                     "assessment_complete": complete})
    paired = []
    for case in cases["cases"]:
        if not case.get("paired_with") or not case["forbidden"]:
            continue
        for variant in ("baseline", "candidate"):
            before = next((r for r in rows if r["case"] == case["paired_with"] and r["variant"] == variant), None)
            after = next((r for r in rows if r["case"] == case["id"] and r["variant"] == variant), None)
            labels = {i["id"] for i in case["forbidden"]}
            passed = None
            if before and after and before["assessment_complete"] and after["assessment_complete"]:
                passed = labels <= set(before["expected_found"]) and not (labels & set(after["forbidden_reported"]))
            paired.append({"case": case["id"], "variant": variant, "found_before_absent_after": passed})
    return {"assessments": rows, "fixed_issue_pairs": paired, "missing_runs": key["missing"],
            "assessment_complete": bool(rows) and not key["missing"] and all(r["assessment_complete"] for r in rows),
            "evidence_verification": "human_attestation_only",
            "note": "Completion records human judgments and coverage attestations, not independently verified evidence. This report never infers quality from comment counts or auto-approves deployment."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("prepare", "score"))
    parser.add_argument("--cases", type=Path, required=True)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--seed", type=int, default=0)
    args = parser.parse_args()
    if args.mode == "prepare":
        if not args.manifest:
            parser.error("prepare requires --manifest")
        report = prepare(read(args.cases), read(args.manifest), args.directory, args.seed)
    else:
        report = score(read(args.cases), read(args.directory / "packets.json"),
                       read(args.directory / "key.json"), read(args.directory / "judgments.json"))
        (args.directory / "quality.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
