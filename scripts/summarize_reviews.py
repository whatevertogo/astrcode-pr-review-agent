#!/usr/bin/env python3
"""Summarize measured paired runs; finding counts are not quality judgments."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import re


def measured(result, directory):
    stages = result["stages"]
    fields = ("input_tokens", "cached_input_tokens", "uncached_input_tokens", "output_tokens",
              "requests", "tool_calls", "estimated_requests", "unknown_accounting_requests", "missing_usage_requests")
    totals = {field: sum(stage["usage"][field] for stage in stages) for field in fields}
    totals["total_tokens"] = totals["input_tokens"] + totals["output_tokens"]
    totals["model_stage_seconds"] = sum(stage["elapsed_seconds"] for stage in stages)
    coverage = result["review"].get("coverage") or {"entries": {}}
    totals["reviewed_paths"] = sorted(path for path, entry in coverage["entries"].items() if entry["status"] == "Reviewed")
    totals["reported_reviewed_paths"] = totals["reviewed_paths"]
    totals["coverage_evidence_complete"] = True
    if result["pipeline"] == "baseline":
        # e86dd4a pre-marks code files Reviewed and refuses later downgrades.
        # Reconstruct coverage from actual file-pass receipts, not that counter.
        debug = Path(result["review"]["debug_dir"])
        relative = Path(*debug.parts[debug.parts.index("agent-data"):])
        observed = set()
        for response in (directory / relative).glob("file-pass-*-response.json"):
            output = json.loads(response.read_text())
            prompt = response.with_name(response.name.replace("-response.json", "-prompt.md"))
            if prompt.exists():
                assigned = set(re.findall(r"^--- file: (.+?) status=", prompt.read_text(), re.MULTILINE))
                observed.update(assigned.intersection(output.get("files_reviewed", [])))
            else:
                totals["coverage_evidence_complete"] = False
        totals["reviewed_paths"] = sorted(observed.intersection(totals["reported_reviewed_paths"]))
    totals["manifest_files"] = len(coverage["entries"])
    totals["inline_findings"] = len(result["review"]["inline_findings"])
    totals["summary_findings"] = len(result["review"]["summary_findings"])
    totals["observations"] = len(result["review"]["observations"])
    return totals


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--quality-dir", default="quality_first", help="Directory containing the optimized variant")
    args = parser.parse_args()
    pairs = []
    for sample in sorted(args.root.iterdir()):
        if not sample.is_dir():
            continue
        variants = {}
        records = {}
        snapshots = {}
        for variant, directory in (("baseline", "baseline"), ("quality_first", args.quality_dir)):
            path = sample / directory / "result.json"
            if path.exists():
                records[variant] = json.loads(path.read_text())
                variants[variant] = measured(records[variant], sample / directory)
                snapshot = json.loads((sample / directory / "snapshot.json").read_text())
                snapshots[variant] = hashlib.sha256(json.dumps(snapshot, sort_keys=True, ensure_ascii=False).encode()).hexdigest()
        if not variants:
            continue
        pair = {"sample": sample.name, "variants": variants, "comparable_coverage": False}
        if len(variants) == 2:
            old, new = records["baseline"], records["quality_first"]
            a, b = variants["baseline"], variants["quality_first"]
            pair["same_code_and_model"] = all(old[field] == new[field] for field in ("repo", "pr_number", "base_sha", "head_sha", "model"))
            pair["same_frozen_context"] = snapshots["baseline"] == snapshots["quality_first"]
            pair["snapshot_digests"] = snapshots
            pair["comparable_coverage"] = pair["same_code_and_model"] and pair["same_frozen_context"] and all(v["coverage_evidence_complete"] for v in (a, b)) and a["reviewed_paths"] == b["reviewed_paths"]
            pair["head_sha"] = new["head_sha"]
            pair["base_sha"] = new["base_sha"]
            pair["native_usage_complete"] = all(v[k] == 0 for v in (a, b) for k in ("estimated_requests", "unknown_accounting_requests", "missing_usage_requests"))
            for key in ("total_tokens", "uncached_input_tokens", "model_stage_seconds"):
                pair[key + "_reduction"] = 1 - b[key] / a[key] if a[key] else None
        pairs.append(pair)
    comparable = [p["total_tokens_reduction"] for p in pairs if p["comparable_coverage"] and p["native_usage_complete"]]
    report = {"pairs": pairs, "quality_directory": args.quality_dir, "comparable_pair_count": len(comparable),
              "median_total_token_reduction": statistics.median(comparable) if comparable else None,
              "quality_gate": "requires independent source adjudication; never inferred from finding counts"}
    args.output_dir.mkdir(parents=True, exist_ok=True)
    (args.output_dir / "metrics.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))
    lines = ["# PR review paired measurements", "", "Only completed result artifacts appear below; partial coverage and failed checks remain visible in each run report.", "",
             "| Sample | Pipeline | Reviewed / manifest | Total tokens | Cached input | Uncached input | Model requests | Stage seconds | Inline findings |",
             "|---|---|---:|---:|---:|---:|---:|---:|---:|"]
    for pair in pairs:
        for variant, v in pair["variants"].items():
            lines.append(f"| {pair['sample']} | {variant} | {len(v['reviewed_paths'])} / {v['manifest_files']} | {v['total_tokens']:,} | {v['cached_input_tokens']:,} | {v['uncached_input_tokens']:,} | {v['requests']} | {v['model_stage_seconds']} | {v['inline_findings']} |")
    lines += ["", f"Comparable-coverage pairs with complete native usage: {len(comparable)}."]
    if comparable:
        lines.append(f"Median total-token reduction within those pairs: {statistics.median(comparable):.1%}.")
    lines += ["", "Baseline coverage is reconstructed from successful file-pass responses intersected with their assigned prompt files: the original coverage counter incorrectly pre-marks code as Reviewed. Generated/oversized files are not credited as fully reviewed. Cached input is already included in total input. Reasoning output is already included in output. Stage time excludes checkout and frozen check preparation; it includes model/tool work and polling. Dollar cost and review precision/recall require separate evidence.", ""]
    (args.output_dir / "comparison.md").write_text("\n".join(lines))


if __name__ == "__main__":
    main()
