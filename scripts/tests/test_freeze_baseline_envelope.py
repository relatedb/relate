import unittest
from pathlib import Path

from comparator_runner import FreezeRun, run_freeze
from loadtest_fixtures import ENV, LOCAL_ENV, memory_report, report

ROOT = Path(__file__).resolve().parents[2]
ENVELOPE = ROOT / "scripts" / "freeze-baseline-envelope.py"


class FreezeEnvelopeTests(unittest.TestCase):
    def run_envelope(self, reports: list[dict]) -> FreezeRun:
        return run_freeze(ENVELOPE, reports)

    def test_requires_environment(self) -> None:
        rep = report(ops_per_sec=10, p99=1, ops=[1, 2, 3], p50=1)
        del rep["environment"]
        result = self.run_envelope([rep, dict(rep)])
        self.assertEqual(result.process.returncode, 1)
        self.assertIn("missing environment", result.process.stderr)

    def test_rejects_environment_mismatch(self) -> None:
        a = report(ops_per_sec=10, p99=1, ops=[1, 2, 3], p50=1)
        b = report(
            ops_per_sec=9, p99=2, env=LOCAL_ENV, ops=[1, 2, 3], p50=1
        )
        result = self.run_envelope([a, b])
        self.assertEqual(result.process.returncode, 1)
        self.assertIn("environment differs", result.process.stderr)

    def test_copies_environment_and_worst_metrics(self) -> None:
        a = report(ops_per_sec=10, p99=100, ops=[1, 2, 3], p50=1)
        b = report(ops_per_sec=8, p99=250, ops=[1, 2, 3], p50=1)
        result = self.run_envelope([a, b])
        self.assertEqual(result.process.returncode, 0, result.process.stderr)
        out = result.payload
        self.assertEqual(out["environment"], ENV)
        self.assertEqual(out["throughput"]["ops_per_sec"], 8)
        self.assertEqual(out["usecases"][0]["latency_ns"]["p99"], 250)
        self.assertTrue(out["ops_omitted"])

    def test_memory_shape_keeps_the_highest_ratio_and_exact_logical_bytes(self) -> None:
        env = dict(ENV, ops=None, warmup=None, workers=None)
        low = memory_report(logical=1000, ratio=1.5, env=env)
        high = memory_report(logical=1000, ratio=2.5, env=env)
        result = self.run_envelope([low, high])
        self.assertEqual(result.process.returncode, 0, result.process.stderr)
        shapes = { (s["shape"], s["index"]): s for s in result.payload["shapes"] }
        self.assertEqual(shapes[("deep", "none")]["ratio"], 2.5)
        self.assertEqual(shapes[("deep", "none")]["logical_bytes"], 1000)
        self.assertEqual(result.payload["baseline_envelope"]["rss_ratio"], "max")

    def test_memory_shape_rejects_logical_byte_drift(self) -> None:
        env = dict(ENV, ops=None, warmup=None, workers=None)
        a = memory_report(logical=1000, ratio=1.5, env=env)
        b = memory_report(ratio=1.6, env=env, logical=1001)
        result = self.run_envelope([a, b])
        self.assertEqual(result.process.returncode, 1)
        self.assertIn("logical_bytes", result.process.stderr)


if __name__ == "__main__":
    unittest.main()
