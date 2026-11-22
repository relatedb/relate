"""The memory-controls envelope and its regression / improvement verdicts."""

import unittest
from pathlib import Path

from comparator_runner import frozen_envelope, run_freeze, run_memory_comparator

ROOT = Path(__file__).resolve().parents[2]
FREEZE = ROOT / "scripts" / "freeze-memory-controls.py"
COMPARE = ROOT / "scripts" / "compare-memory-controls.py"

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
    "graph_scale": 10,
    "workers": 16,
    "read_parts": 80,
    "write_parts": 20,
    "max_connections": 0,
    "concurrent_logical_workspace_limit": 55829120,
    "ops": 40,
    "warmup_ops": 8,
    "seed": 1,
    "index_mode": "both",
    "read_query": "mix",
}


def case(name, kind, failure, outcome, rss=10_000_000, processing=1_000_000, session=100_000):
    return {
        "kind": kind,
        "name": name,
        "scenario": {
            "given": f"given {name}",
            "when": f"when {name}",
            "then": f"then {name}",
            "outcome": outcome,
        },
        "baseline_rss_bytes": rss // 2,
        "cold_rss_bytes": rss,
        "cold_hwm_bytes": rss,
        "floor_rss_bytes": rss // 2,
        "graph_logical_bytes": 3_000_000,
        "processing_rss_min_bytes": processing,
        "processing_rss_max_bytes": processing,
        "peak_hwm_bytes": rss,
        "session_cost_bytes": session,
        "max_connections": 0,
        "idle_rss_bytes": [rss, rss],
        "failure": failure,
    }


def workload(name, limited=4, succeeded=30):
    body = case(name, "control", "configured workload", f"outcome {name}")
    return {
        "scenario": body["scenario"],
        "case": body,
        "index_mode": "indexed" if name.endswith("indexed") else "scan",
        "reads_succeeded": succeeded,
        "reads_limited": limited,
        "reads_failed": 0,
        "writes_succeeded": succeeded,
        "writes_limited": limited,
        "writes_failed": 0,
        "connections_refused": 0,
        "active_query_refusals": 0,
    }


def summary(family, slope=2.0, ratio=3.0, holder=4096.0):
    return {
        "family": family,
        "rss_single_holder_slope": slope,
        "max_rss_to_logical_ratio": ratio,
        "max_cgroup_to_logical_ratio": ratio,
        "rss_per_holder_bytes": holder,
    }


def report(**overrides):
    cap = case(
        "connection-cap",
        "control",
        "accept-then-close",
        "the third connection was closed and the log named the cap. A later connection was accepted.",
    )
    unlimited = case(
        "unlimited-connections",
        "control",
        "opened",
        "connection 129 was accepted, and the first session read an account and wrote a review.",
    )
    workspace = case(
        "concurrent-logical-workspace-limit",
        "control",
        "resource limit exceeded: concurrent-logical-workspace-limit limit=14043",
        "the overlapping write was refused (resource limit exceeded: concurrent-logical-workspace-limit limit=14043).",
    )
    calibration = case(
        "workspace-memory-calibration",
        "calibration",
        "cross-family logical-byte-to-RSS calibration matrix",
        "the displayed observation held 1 logical bytes and retained 1 RSS bytes (1 RSS bytes per logical byte).",
    )
    body = {
        "schema_version": 4,
        "name": "memory-controls",
        "environment": dict(ENV),
        "controls": dict(KNOBS),
        "cases": [cap, unlimited, workspace, calibration],
        "workloads": [workload("workload-indexed"), workload("workload-scan")],
        "workspace_memory_calibration": {
            "repetitions": 3,
            "observations": [],
            "summaries": [summary("sort"), summary("expand"), summary("overall")],
        },
    }
    body.update(overrides)
    return body


class CompareMemoryControlsTests(unittest.TestCase):
    def run_compare(self, baseline, current, env=None):
        return run_memory_comparator(
            COMPARE, baseline, current, extra_env=env
        )

    def freeze(self, rounds):
        return run_freeze(FREEZE, rounds)

    def frozen(self, rounds):
        return frozen_envelope(self.freeze(rounds))

    def test_ok(self):
        result = self.run_compare(self.frozen([report(), report()]), report())
        self.assertEqual(result.process.returncode, 0, result.process.stderr)
        self.assertEqual(result.verdict["status"], "ok")
        self.assertIn("**Status:** ok", result.document)
        self.assertIn("All memory controls and workspace memory calibrations held.", result.document)

    def test_regression(self):
        payload = self.frozen([report(), report()])
        current = report()
        current["cases"][0]["baseline_rss_bytes"] = 20_000_000
        current["cases"][0]["floor_rss_bytes"] = 20_000_000
        result = self.run_compare(payload, current, env={"GITHUB_ACTIONS": ""})
        self.assertEqual(result.process.returncode, 0, result.process.stderr)
        self.assertEqual(result.verdict["status"], "regression")
        self.assertIn("**Given:**", result.document)
        self.assertIn("38.15 MB", result.document)
        self.assertIn("9.54 MB", result.document)
        self.assertIn("job stays green", result.document)
        self.assertNotIn("::warning::", result.process.stderr)
        warned = self.run_compare(payload, current, env={"GITHUB_ACTIONS": "true"})
        self.assertEqual(warned.process.returncode, 0, warned.process.stderr)
        self.assertIn("::warning::compare-memory-controls:", warned.process.stderr)
        self.assertIn("regression", warned.process.stderr)

    def test_improvement(self):
        payload = self.frozen([report(), report()])
        current = report()
        current["cases"][0]["baseline_rss_bytes"] = 1_000_000
        current["cases"][0]["floor_rss_bytes"] = 1_000_000
        current["cases"][0]["processing_rss_min_bytes"] = 1_000_000
        current["cases"][0]["processing_rss_max_bytes"] = 1_000_000
        current["cases"][0]["session_cost_bytes"] = 100_000
        result = self.run_compare(payload, current, env={"GITHUB_ACTIONS": ""})
        self.assertEqual(result.process.returncode, 0, result.process.stderr)
        self.assertEqual(result.verdict["status"], "improvement")
        self.assertIn("create-loadtest-baselines", result.document)
        self.assertIn("improvement", result.process.stderr)
        self.assertNotIn("::warning::", result.process.stderr)
        warned = self.run_compare(payload, current, env={"GITHUB_ACTIONS": "true"})
        self.assertEqual(warned.process.returncode, 0, warned.process.stderr)
        self.assertIn("::warning::compare-memory-controls:", warned.process.stderr)
        self.assertIn("improvement", warned.process.stderr)

    def test_mixed(self):
        payload = self.frozen([report(), report()])
        current = report()
        current["cases"][0]["baseline_rss_bytes"] = 20_000_000
        current["cases"][0]["floor_rss_bytes"] = 20_000_000
        current["workloads"][0]["reads_limited"] = 0
        current["workloads"][0]["writes_limited"] = 0
        result = self.run_compare(payload, current, env={"GITHUB_ACTIONS": "true"})
        self.assertEqual(result.process.returncode, 0, result.process.stderr)
        self.assertEqual(result.verdict["status"], "mixed")
        self.assertIn("::warning::", result.process.stderr)

    def test_incompatible_is_local_skip_and_ci_failure(self):
        payload = self.frozen([report(), report()])
        current = report()
        current["environment"] = LOCAL_ENV
        current["cases"][0]["baseline_rss_bytes"] = 20_000_000
        current["cases"][0]["floor_rss_bytes"] = 20_000_000
        # The one parametrized policy case: a local run (CI_COMPARE=0)
        # skips the bands and exits 0, a CI run (CI_COMPARE=1) hard-fails.
        for env, code in (({"CI_COMPARE": "0", "GITHUB_ACTIONS": ""}, 0), ({"CI_COMPARE": "1"}, 1)):
            with self.subTest(env=env):
                result = self.run_compare(payload, current, env=env)
                self.assertEqual(result.process.returncode, code, result.process.stderr)
                self.assertEqual(result.verdict["status"], "incompatible")

    def test_no_baseline(self):
        result = self.run_compare({"schema_version": 1, "name": "memory-controls", "frozen": False}, report())
        self.assertEqual(result.process.returncode, 0, result.process.stderr)
        self.assertEqual(result.verdict["status"], "no-baseline")
        self.assertIn("**Status:** no-baseline", result.document)

    def test_broken_behaviour_is_a_regression(self):
        payload = self.frozen([report(), report()])
        current = report()
        current["cases"][2]["failure"] = "the overlapping write was admitted"
        result = self.run_compare(payload, current)
        self.assertEqual(result.process.returncode, 1)
        self.assertEqual(result.verdict["status"], "regression")
        self.assertTrue(
            any(row["name"] == "concurrent-logical-workspace-limit" and row["verdict"] == "regression" for row in result.verdict["checks"])
        )

    def test_ratio_regression_states_bounds_and_measured_size(self):
        payload = self.frozen([report(), report()])
        current = report()
        current["workspace_memory_calibration"]["summaries"][0]["max_rss_to_logical_ratio"] = 3.2
        current["workspace_memory_calibration"]["observations"] = [
            {
                "family": "sort",
                "rss_delta_bytes": 16_000_000,
                "rss_to_logical_ratio": 3.2,
                "cgroup_delta_bytes": 12_000_000,
                "cgroup_to_logical_ratio": 3.0,
            }
        ]
        result = self.run_compare(payload, current)
        self.assertEqual(result.process.returncode, 0, result.process.stderr)
        self.assertEqual(result.verdict["status"], "regression")
        self.assertIn("Expected RSS ratio 3.000x–3.000x.", result.document)
        self.assertIn("A reading may sit 5% outside that range.", result.document)
        self.assertIn("Measured 15.26 MB, ratio 3.200x.", result.document)
        self.assertNotIn("rss_per_holder", result.document)

    def test_freeze_refuses_a_drifting_limit(self):
        drifted = report()
        drifted["cases"][2]["failure"] = drifted["cases"][2]["failure"].replace("14043", "14044")
        result = self.freeze([report(), drifted])
        self.assertEqual(result.process.returncode, 1)
        self.assertIn("behaviour differs", result.process.stderr)


if __name__ == "__main__":
    unittest.main()
