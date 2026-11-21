import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from loadtest_fixtures import ENV, LOCAL_ENV, memory_report, report

ROOT = Path(__file__).resolve().parents[2]
ENVELOPE = ROOT / "scripts" / "freeze-baseline-envelope.py"


class FreezeEnvelopeTests(unittest.TestCase):
    def run_envelope(self, reports: list[dict]) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as tmp:
            paths = []
            for i, rep in enumerate(reports):
                path = Path(tmp) / f"run{i}.json"
                path.write_text(json.dumps(rep))
                paths.append(path)
            out = Path(tmp) / "out.json"
            cmd = [sys.executable, str(ENVELOPE), "--out", str(out), *[str(p) for p in paths]]
            result = subprocess.run(cmd, text=True, capture_output=True, check=False)
            if result.returncode == 0:
                result.payload = json.loads(out.read_text())  # type: ignore[attr-defined]
            return result

    def test_requires_environment(self) -> None:
        rep = report(ops_per_sec=10, p99=1, ops=[1, 2, 3], p50=1)
        del rep["environment"]
        result = self.run_envelope([rep, dict(rep)])
        self.assertEqual(result.returncode, 1)
        self.assertIn("missing environment", result.stderr)

    def test_rejects_environment_mismatch(self) -> None:
        a = report(ops_per_sec=10, p99=1, ops=[1, 2, 3], p50=1)
        b = report(
            ops_per_sec=9, p99=2, env=LOCAL_ENV, ops=[1, 2, 3], p50=1
        )
        result = self.run_envelope([a, b])
        self.assertEqual(result.returncode, 1)
        self.assertIn("environment differs", result.stderr)

    def test_copies_environment_and_worst_metrics(self) -> None:
        a = report(ops_per_sec=10, p99=100, ops=[1, 2, 3], p50=1)
        b = report(ops_per_sec=8, p99=250, ops=[1, 2, 3], p50=1)
        result = self.run_envelope([a, b])
        self.assertEqual(result.returncode, 0, result.stderr)
        out = result.payload  # type: ignore[attr-defined]
        self.assertEqual(out["environment"], ENV)
        self.assertEqual(out["throughput"]["ops_per_sec"], 8)
        self.assertEqual(out["usecases"][0]["latency_ns"]["p99"], 250)
        self.assertTrue(out["ops_omitted"])

    def test_memory_shape_keeps_the_highest_ratio_and_exact_logical_bytes(self) -> None:
        env = dict(ENV, ops=None, warmup=None, workers=None)
        low = memory_report(logical=1000, ratio=1.5, env=env)
        high = memory_report(logical=1000, ratio=2.5, env=env)
        result = self.run_envelope([low, high])
        self.assertEqual(result.returncode, 0, result.stderr)
        shapes = { (s["shape"], s["index"]): s for s in result.payload["shapes"] }
        self.assertEqual(shapes[("deep", "none")]["ratio"], 2.5)
        self.assertEqual(shapes[("deep", "none")]["logical_bytes"], 1000)
        self.assertEqual(result.payload["baseline_envelope"]["rss_ratio"], "max")

    def test_memory_shape_rejects_logical_byte_drift(self) -> None:
        env = dict(ENV, ops=None, warmup=None, workers=None)
        a = memory_report(logical=1000, ratio=1.5, env=env)
        b = memory_report(ratio=1.6, env=env, logical=1001)
        result = self.run_envelope([a, b])
        self.assertEqual(result.returncode, 1)
        self.assertIn("logical_bytes", result.stderr)


if __name__ == "__main__":
    unittest.main()
