"""The memory-shape envelope and its regression / improvement verdicts."""

import unittest
from pathlib import Path

from comparator_runner import frozen_envelope, run_freeze, run_memory_comparator

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

# The mismatched-environment fixture: ENV under a `local` runner,
# derived beside ENV so the mismatch's shape is one edit.
LOCAL_ENV = dict(ENV, runner="local")

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
    ("mid degree", 1_184, 113_664),
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
        return run_memory_comparator(
            COMPARE, baseline, current, extra_env=env
        )

    def freeze(self, rounds):
        return run_freeze(FREEZE, rounds)

    def frozen(self, rounds):
        return frozen_envelope(self.freeze(rounds))

    def test_ok_states_baseline_validated_and_report(self):
        result = self.run_compare(self.frozen([report(), report()]), report())
        self.assertEqual(result.process.returncode, 0, result.process.stderr)
        self.assertEqual(result.verdict["status"], "ok")
        self.assertIn("**Baseline:**", result.document)
        self.assertIn("**Validated:**", result.document)
        self.assertIn("**Report:** logical, disk, attributed RSS, and the ratio held.", result.document)
        self.assertIn("31.47 MB", result.document)

    def test_byte_regression(self):
        payload = self.frozen([report(), report()])
        current = report()
        current["shapes"][1]["attributed_rss_bytes"] = 60_000_000
        result = self.run_compare(payload, current, env={"GITHUB_ACTIONS": ""})
        self.assertEqual(result.process.returncode, 0, result.process.stderr)
        self.assertEqual(result.verdict["status"], "regression")
        self.assertIn("**Given:**", result.document)
        self.assertIn("57.22 MB", result.document)
        self.assertIn("44.82 MB", result.document)
        self.assertIn("job stays green", result.document)
        warned = self.run_compare(payload, current, env={"GITHUB_ACTIONS": "true"})
        self.assertEqual(warned.process.returncode, 0, warned.process.stderr)
        self.assertIn("::warning::compare-memory-shape:", warned.process.stderr)

    def test_ratio_regression(self):
        payload = self.frozen([report(), report()])
        current = report()
        current["shapes"][1]["ratio"] = 1.6
        result = self.run_compare(payload, current)
        self.assertEqual(result.process.returncode, 0, result.process.stderr)
        self.assertEqual(result.verdict["status"], "regression")
        self.assertIn("ratio measured 1.600x", result.document)

    def test_improvement(self):
        payload = self.frozen([report(), report()])
        current = report()
        current["shapes"][1]["logical_bytes"] = 20_000_000
        result = self.run_compare(payload, current, env={"GITHUB_ACTIONS": ""})
        self.assertEqual(result.process.returncode, 0, result.process.stderr)
        self.assertEqual(result.verdict["status"], "improvement")
        self.assertIn("create-loadtest-baselines", result.document)
        self.assertIn("improvement", result.process.stderr)
        self.assertNotIn("::warning::", result.process.stderr)
        warned = self.run_compare(payload, current, env={"GITHUB_ACTIONS": "true"})
        self.assertEqual(warned.process.returncode, 0, warned.process.stderr)
        self.assertIn("::warning::compare-memory-shape:", warned.process.stderr)
        self.assertIn("improvement", warned.process.stderr)

    def test_wrong_count_is_a_regression(self):
        payload = self.frozen([report(), report()])
        current = report()
        current["shapes"][1]["nodes"] = 130_000
        result = self.run_compare(payload, current)
        self.assertEqual(result.process.returncode, 1)
        self.assertEqual(result.verdict["status"], "regression")
        self.assertIn("130000 nodes", result.document)
        self.assertIn("120000 nodes", result.document)

    def test_incompatible_is_local_skip_and_ci_failure(self):
        payload = self.frozen([report(), report()])
        current = report()
        current["environment"] = LOCAL_ENV
        current["shapes"][1]["attributed_rss_bytes"] = 60_000_000
        # The one parametrized policy case: a local run (CI_COMPARE=0)
        # skips the bands and exits 0, a CI run (CI_COMPARE=1) hard-fails.
        for env, code in (({"CI_COMPARE": "0", "GITHUB_ACTIONS": ""}, 0), ({"CI_COMPARE": "1"}, 1)):
            with self.subTest(env=env):
                result = self.run_compare(payload, current, env=env)
                self.assertEqual(result.process.returncode, code, result.process.stderr)
                self.assertEqual(result.verdict["status"], "incompatible")

    def test_no_baseline(self):
        result = self.run_compare({"schema_version": 1, "name": "memory-shape", "frozen": False}, report())
        self.assertEqual(result.process.returncode, 0, result.process.stderr)
        self.assertEqual(result.verdict["status"], "no-baseline")
        self.assertIn("**Status:** no-baseline", result.document)

    def test_freeze_refuses_a_different_count(self):
        drifted = report()
        drifted["shapes"][1]["nodes"] = 130_000
        result = self.freeze([report(), drifted])
        self.assertEqual(result.process.returncode, 1)
        self.assertIn("120000", result.process.stderr)


if __name__ == "__main__":
    unittest.main()
