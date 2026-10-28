import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

from evaluate_reviews import prepare, score


class EvaluationTest(unittest.TestCase):
    def test_blinding_identity_checks_and_before_after_gate(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cases = {"cases": [
                {"id": "before", "repo": "o/r", "pr": 1, "head": "a" * 40,
                 "expected": [{"id": "bug"}], "forbidden": []},
                {"id": "after", "repo": "o/r", "pr": 1, "head": "b" * 40,
                 "expected": [], "forbidden": [{"id": "bug"}], "paired_with": "before"}]}
            manifest = {"binary_sha256": {"baseline": "old", "candidate": "new"}, "runs": []}
            for case in cases["cases"]:
                snapshot = root / (case["id"] + "-input.json")
                snapshot.write_text(json.dumps({"head": case["head"], "context": {}}))
                for variant in ("baseline", "candidate"):
                    directory = root / (case["id"] + variant)
                    directory.mkdir()
                    saved_snapshot = json.loads(snapshot.read_text())
                    if variant == "candidate":
                        saved_snapshot["context"]["revision"] = None
                    (directory / "snapshot.json").write_text(json.dumps(saved_snapshot))
                    review = {"inline_findings": [{"severity": "P3", "title": "bug"}] if case["id"] == "before" else [],
                              "coverage": {"entries": {"a.rs": {"status": "Reviewed"}}}}
                    (directory / "result.json").write_text(json.dumps({"repo": "o/r", "pr_number": 1,
                        "base_sha": "base", "head_sha": case["head"], "status": "complete", "model": {"name": "same"}, "review": review}))
                    manifest["runs"].append({"sample": f"o/r#1@{case['head']}", "variant": variant,
                        "binary_sha256": manifest["binary_sha256"][variant], "status": "complete", "exit_code": 0,
                        "output_dir": str(directory), "worktree_head": case["head"], "tracked_worktree_clean": True,
                        "snapshot_path": str(snapshot),
                        "snapshot_sha256": hashlib.sha256(snapshot.read_bytes()).hexdigest()})
            directory = root / "blind"
            self.assertEqual(prepare(cases, manifest, directory)["missing"], [])
            packets = json.loads((directory / "packets.json").read_text())
            self.assertNotIn('"variant"', json.dumps(packets))
            self.assertNotIn('"case"', json.dumps(packets))
            key = json.loads((directory / "key.json").read_text())
            judgments = json.loads((directory / "judgments.json").read_text())
            self.assertFalse(score(cases, packets, key, judgments)["assessment_complete"])
            for item in judgments.values():
                item.update(coverage_checked=True, coverage_evidence="reviewed source a.rs at frozen head")
                for finding in item["findings"]:
                    finding.update(verdict="true_positive", issue_ids=["bug"], evidence="a.rs:10 proves the trigger")
            result = score(cases, packets, key, judgments)
            self.assertTrue(result["assessment_complete"])
            self.assertTrue(all(p["found_before_absent_after"] for p in result["fixed_issue_pairs"]))
            incomplete = copy.deepcopy(packets)
            incomplete[0]["coverage"]["entries"]["a.rs"]["status"] = "Failed"
            self.assertFalse(score(cases, incomplete, key, judgments)["assessment_complete"])
            bad = copy.deepcopy(judgments)
            before = next(p["id"] for p in packets if key["mapping"][p["id"]]["case"] == "before")
            bad[before]["findings"][0]["evidence"] = ""
            with self.assertRaises(ValueError):
                score(cases, packets, key, bad)
            bad = copy.deepcopy(judgments)
            bad[before]["findings"][0]["issue_ids"] = []
            with self.assertRaises(ValueError):
                score(cases, packets, key, bad)
            regressed = copy.deepcopy(packets)
            after = next(p for p in regressed if key["mapping"][p["id"]]["case"] == "after")
            after["findings"] = [{"id": "F001", "finding": {"title": "fixed bug still reported"}}]
            bad = copy.deepcopy(judgments)
            bad[after["id"]]["findings"] = [{"id": "F001", "verdict": "false_positive",
                "issue_ids": ["bug"], "evidence": "a.rs:10 now guards the failure"}]
            result = score(cases, regressed, key, bad)
            pair = next(p for p in result["fixed_issue_pairs"] if p["variant"] == key["mapping"][after["id"]]["variant"])
            self.assertFalse(pair["found_before_absent_after"])
            self.assertEqual(result["evidence_verification"], "human_attestation_only")
            for record in manifest["runs"]:
                if record["sample"].endswith("@" + cases["cases"][1]["head"]):
                    path = Path(record["output_dir"]) / "result.json"
                    original = json.loads(path.read_text())
                    changed = copy.deepcopy(original)
                    changed["model"] = {"name": "different"}
                    path.write_text(json.dumps(changed))
            with self.assertRaisesRegex(ValueError, "before/after"):
                prepare(cases, manifest, root / "different-models")
            snapshot = Path(manifest["runs"][0]["snapshot_path"])
            snapshot.write_text("{}")
            with self.assertRaisesRegex(ValueError, "snapshot changed"):
                prepare(cases, manifest, root / "tampered")


if __name__ == "__main__":
    unittest.main()
