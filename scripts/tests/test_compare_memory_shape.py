"""The memory-shape envelope and its regression / improvement verdicts."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FREEZE = ROOT / "scripts" / "freeze-memory-shape.py"
COMPARE = ROOT / "scripts" / "compare-memory-shape.py"

ENV = {
    "class": "relate-loadtest-baseline/v1",
    "arch": "aarch64",
    "os": "linux",
    "build_image": "rust:1.88-bookworm@sha256:abc",
    "run_image": "debian:bookworm-slim@sha256:def",
    "cpus": 2,
    "cpu_period_us": 10000,
    "cpu_quota_us": 20000,
    "memory": "2g",
    "ops": 40,
    "warmup": 8,
    "workers": 16,
    "runner": "github-actions:linux-4-core-x86",
}

KNOBS = {
    "logical_target_bytes": 33_554_432,
    "attributes": 1,
    "text_indexes": 0,
    "point_indexes": 0,
    "range_indexes": 0,
}

COUNTS = (
    ("base", 0, 0),
    ("flat", 120_000, 0),
    ("fanout", 52_016, 52_000),
    ("ring", 46_000, 46_000),
    ("dense ring", 5_000, 100_000),
)


def shape(name, nodes, rels, logical=33_000_000, disk=2_000_000, rss=47_000_000, ratio=1.42):
    return {
        "shape": name,
        "index": "none",
        "nodes": nodes,
        "relationships": rels,
        "logical_bytes": logical,
        "disk_bytes": disk,
        "rss_bytes": rss + 11_000_000,
        "peak_rss_bytes": rss + 11_000_000,
        "baseline_rss_bytes": 11_000_000,
        "attributed_rss_bytes": rss,
        "ratio": ratio,
        "scenario": {
            "given": f"given {name}",
            "when": f"when {name}",
            "then": f"then {name}",
            "outcome": f"outcome {name}",
        },
    }


def report(**overrides):
    body = {
        "schema_version": 2,
        "name": "memory-shape",
        "environment": dict(ENV),
        "config": dict(KNOBS),
        "shapes": [shape(name, nodes, rels) for name, nodes, rels in COUNTS],
    }
    body.update(overrides)
    return body


class CompareMemoryShapeTests(unittest.TestCase):
    def run_compare(self, baseline, current, env=None):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            baseline_path = root / "baseline.json"
            report_path = root / "report.json"
            doc = root / "memory-shape.md"
            verdict = root / "verdict.json"
            baseline_path.write_text(json.dumps(baseline))
            report_path.write_text(json.dumps(current))
            result = subprocess.run(
                [
                    sys.executable,
                    str(COMPARE),
                    "--baseline",
                    str(baseline_path),
                    "--report",
                    str(report_path),
                    "--doc",
                    str(doc),
                    "--verdict",
                    str(verdict),
                ],
                text=True,
                capture_output=True,
                check=False,
                env={**os.environ, **(env or {})},
            )
            result.document = doc.read_text()  # type: ignore[attr-defined]
            result.payload = json.loads(verdict.read_text())  # type: ignore[attr-defined]
            return result

    def freeze(self, rounds):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            paths = []
            for index, body in enumerate(rounds):
                path = root / f"run{index}.json"
                path.write_text(json.dumps(body))
                paths.append(str(path))
            out = root / "envelope.json"
            result = subprocess.run(
                [sys.executable, str(FREEZE), "--out", str(out), *paths],
                text=True,
                capture_output=True,
                check=False,
            )
            payload = json.loads(out.read_text()) if result.returncode == 0 else None
            return result, payload

    def test_ok_states_baseline_validated_and_report(self):
        frozen, payload = self.freeze([report(), report()])
        self.assertEqual(frozen.returncode, 0, frozen.stderr)
        result = self.run_compare(payload, report())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.payload["status"], "ok")
        self.assertIn("**Baseline:**", result.document)
        self.assertIn("**Validated:**", result.document)
        self.assertIn("**Report:** logical, disk, attributed RSS, and the ratio held.", result.document)
        self.assertIn("31.47 MB", result.document)

    def test_byte_regression(self):
        _, payload = self.freeze([report(), report()])
        current = report()
        current["shapes"][1]["attributed_rss_bytes"] = 60_000_000
        result = self.run_compare(payload, current)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.payload["status"], "regression")
        self.assertIn("**Given:**", result.document)
        self.assertIn("57.22 MB", result.document)
        self.assertIn("44.82 MB", result.document)

    def test_ratio_regression(self):
        _, payload = self.freeze([report(), report()])
        current = report()
        current["shapes"][1]["ratio"] = 1.6
        result = self.run_compare(payload, current)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.payload["status"], "regression")
        self.assertIn("ratio measured 1.600x", result.document)

    def test_improvement(self):
        _, payload = self.freeze([report(), report()])
        current = report()
        current["shapes"][1]["logical_bytes"] = 20_000_000
        result = self.run_compare(payload, current, env={"GITHUB_ACTIONS": ""})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.payload["status"], "improvement")
        self.assertIn("create-loadtest-baselines", result.document)
        self.assertIn("improvement", result.stderr)
        self.assertNotIn("::warning::", result.stderr)
        warned = self.run_compare(payload, current, env={"GITHUB_ACTIONS": "true"})
        self.assertEqual(warned.returncode, 0, warned.stderr)
        self.assertIn("::warning::compare-memory-shape:", warned.stderr)
        self.assertIn("improvement", warned.stderr)

    def test_wrong_count_is_a_regression(self):
        _, payload = self.freeze([report(), report()])
        current = report()
        current["shapes"][1]["nodes"] = 130_000
        result = self.run_compare(payload, current)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.payload["status"], "regression")
        self.assertIn("130000 nodes", result.document)
        self.assertIn("120000 nodes", result.document)

    def test_incompatible_is_local_skip_and_ci_failure(self):
        _, payload = self.freeze([report(), report()])
        current = report()
        current["environment"] = dict(ENV, runner="local")
        current["shapes"][1]["attributed_rss_bytes"] = 60_000_000
        local = self.run_compare(payload, current, env={"CI_COMPARE": "0", "GITHUB_ACTIONS": ""})
        self.assertEqual(local.returncode, 0, local.stderr)
        self.assertEqual(local.payload["status"], "incompatible")
        ci = self.run_compare(payload, current, env={"CI_COMPARE": "1"})
        self.assertEqual(ci.returncode, 1)
        self.assertEqual(ci.payload["status"], "incompatible")

    def test_no_baseline(self):
        result = self.run_compare({"schema_version": 1, "name": "memory-shape", "frozen": False}, report())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.payload["status"], "no-baseline")
        self.assertIn("**Status:** no-baseline", result.document)

    def test_freeze_refuses_a_different_count(self):
        drifted = report()
        drifted["shapes"][1]["nodes"] = 130_000
        result, _payload = self.freeze([report(), drifted])
        self.assertEqual(result.returncode, 1)
        self.assertIn("120000", result.stderr)


if __name__ == "__main__":
    unittest.main()
