"""The memory-controls envelope and its regression / improvement verdicts."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

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
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            baseline_path = root / "baseline.json"
            report_path = root / "report.json"
            doc = root / "memory-controls.md"
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

    def test_ok(self):
        frozen, payload = self.freeze([report(), report()])
        self.assertEqual(frozen.returncode, 0, frozen.stderr)
        result = self.run_compare(payload, report())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.payload["status"], "ok")
        self.assertIn("**Status:** ok", result.document)
        self.assertIn("All memory controls and workspace memory calibrations held.", result.document)

    def test_regression(self):
        _, payload = self.freeze([report(), report()])
        current = report()
        current["cases"][0]["baseline_rss_bytes"] = 20_000_000
        current["cases"][0]["floor_rss_bytes"] = 20_000_000
        result = self.run_compare(payload, current, env={"GITHUB_ACTIONS": ""})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.payload["status"], "regression")
        self.assertIn("**Given:**", result.document)
        self.assertIn("38.15 MB", result.document)
        self.assertIn("9.54 MB", result.document)
        self.assertIn("job stays green", result.document)
        self.assertNotIn("::warning::", result.stderr)
        warned = self.run_compare(payload, current, env={"GITHUB_ACTIONS": "true"})
        self.assertEqual(warned.returncode, 0, warned.stderr)
        self.assertIn("::warning::compare-memory-controls:", warned.stderr)
        self.assertIn("regression", warned.stderr)

    def test_improvement(self):
        _, payload = self.freeze([report(), report()])
        current = report()
        current["cases"][0]["baseline_rss_bytes"] = 1_000_000
        current["cases"][0]["floor_rss_bytes"] = 1_000_000
        current["cases"][0]["processing_rss_min_bytes"] = 1_000_000
        current["cases"][0]["processing_rss_max_bytes"] = 1_000_000
        current["cases"][0]["session_cost_bytes"] = 100_000
        result = self.run_compare(payload, current, env={"GITHUB_ACTIONS": ""})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.payload["status"], "improvement")
        self.assertIn("create-loadtest-baselines", result.document)
        self.assertIn("improvement", result.stderr)
        self.assertNotIn("::warning::", result.stderr)
        warned = self.run_compare(payload, current, env={"GITHUB_ACTIONS": "true"})
        self.assertEqual(warned.returncode, 0, warned.stderr)
        self.assertIn("::warning::compare-memory-controls:", warned.stderr)
        self.assertIn("improvement", warned.stderr)

    def test_mixed(self):
        _, payload = self.freeze([report(), report()])
        current = report()
        current["cases"][0]["baseline_rss_bytes"] = 20_000_000
        current["cases"][0]["floor_rss_bytes"] = 20_000_000
        current["workloads"][0]["reads_limited"] = 0
        current["workloads"][0]["writes_limited"] = 0
        result = self.run_compare(payload, current, env={"GITHUB_ACTIONS": "true"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.payload["status"], "mixed")
        self.assertIn("::warning::", result.stderr)

    def test_incompatible_is_local_skip_and_ci_failure(self):
        _, payload = self.freeze([report(), report()])
        current = report()
        current["environment"] = dict(ENV, runner="local")
        current["cases"][0]["baseline_rss_bytes"] = 20_000_000
        current["cases"][0]["floor_rss_bytes"] = 20_000_000
        local = self.run_compare(payload, current, env={"CI_COMPARE": "0", "GITHUB_ACTIONS": ""})
        self.assertEqual(local.returncode, 0, local.stderr)
        self.assertEqual(local.payload["status"], "incompatible")
        ci = self.run_compare(payload, current, env={"CI_COMPARE": "1"})
        self.assertEqual(ci.returncode, 1)
        self.assertEqual(ci.payload["status"], "incompatible")

    def test_no_baseline(self):
        result = self.run_compare({"schema_version": 1, "name": "memory-controls", "frozen": False}, report())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.payload["status"], "no-baseline")
        self.assertIn("**Status:** no-baseline", result.document)

    def test_broken_behaviour_is_a_regression(self):
        _, payload = self.freeze([report(), report()])
        current = report()
        current["cases"][2]["failure"] = "the overlapping write was admitted"
        result = self.run_compare(payload, current)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.payload["status"], "regression")
        self.assertTrue(
            any(row["name"] == "concurrent-logical-workspace-limit" and row["verdict"] == "regression" for row in result.payload["checks"])
        )

    def test_ratio_regression_states_bounds_and_measured_size(self):
        _, payload = self.freeze([report(), report()])
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
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.payload["status"], "regression")
        self.assertIn("Expected RSS ratio 3.000x–3.000x.", result.document)
        self.assertIn("A reading may sit 5% outside that range.", result.document)
        self.assertIn("Measured 15.26 MB, ratio 3.200x.", result.document)
        self.assertNotIn("rss_per_holder", result.document)

    def test_a_slope_outside_the_envelope_is_not_a_regression(self):
        _, payload = self.freeze([report(), report()])
        payload["calibration"]["sort"]["rss_per_holder_bytes"] = {"min": -1, "max": 1}
        result = self.run_compare(payload, report())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.payload["status"], "ok")
        self.assertIn("Expected RSS ratio 3.000x–3.000x.", result.document)

    def test_freeze_refuses_a_drifting_limit(self):
        drifted = report()
        drifted["cases"][2]["failure"] = drifted["cases"][2]["failure"].replace("14043", "14044")
        result, _payload = self.freeze([report(), drifted])
        self.assertEqual(result.returncode, 1)
        self.assertIn("behaviour differs", result.stderr)


if __name__ == "__main__":
    unittest.main()
