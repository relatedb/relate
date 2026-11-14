"""Unit-test the ADR-0245 query-latency gate: the worst-of-N envelope
freeze (scripts/freeze-query-latency-envelope.py) and the per-query p50
compare (scripts/compare-query-latency.py).

The compare is also exercised at module level (the verdict layer), the
way test_compare_verdict.py exercises the loadtest comparator, plus
subprocess exit codes for the gate's fail/incompatible/ok paths.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from comparator_runner import run_comparator, scrubbed_env
from script_loader import ROOT, load

FREEZE = ROOT / "scripts" / "freeze-query-latency-envelope.py"
COMPARE = ROOT / "scripts" / "compare-query-latency.py"

# The compare module reads its band knobs from the environment at import
# time (QUERY_LATENCY_* constants), and the module-level exercises below
# import it into this process — so the load runs inside the one scrub
# policy's environment (scrubbed_env), scoped to the import itself; the
# subprocess exercises get the same scrub via scrubbed_env.
with mock.patch.dict(os.environ, scrubbed_env(), clear=True):
    ql_compare = load("compare_query_latency", COMPARE)

ENV = {
    "class": "relate-loadtest-baseline/v1",
    "runner": "github-actions:ubuntu-24.04",
    "memory": "6g",
}


def query(qid, profile, p50_ns, mean_ns=None, status="ok", detail=None):
    """One QueryOutcome in the suite report's shape (engine Stats)."""
    return {
        "qid": qid,
        "index_profile": profile,
        "status": status,
        "detail": detail,
        "rows": 1,
        "rounds": 100,
        "engine": {
            "p50_ns": p50_ns,
            "mean_ns": mean_ns if mean_ns is not None else p50_ns,
            "min_ns": p50_ns,
            "p99_ns": p50_ns,
            "max_ns": p50_ns,
        },
    }


def report(queries, environment=ENV, peak_rss=3_000_000_000):
    return {
        "schema": 1,
        "suite": "graph-benchmark-ldbc",
        "sf": "0.1",
        "index_profile": "upstream",
        "engine_version": "0.12.4",
        "environment": environment,
        "peak_rss_bytes": peak_rss,
        "graph": {"origin": "loaded", "store": ":memory:", "load": {"seconds": 61.7}},
        "queries": queries,
    }


class QueryLatencyFreezeTests(unittest.TestCase):
    def run_freeze(self, reports):
        with tempfile.TemporaryDirectory() as tmp:
            paths = []
            for i, rep in enumerate(reports):
                path = Path(tmp) / f"run{i}.json"
                path.write_text(json.dumps(rep))
                paths.append(path)
            out = Path(tmp) / "out.json"
            result = subprocess.run(
                [sys.executable, str(FREEZE), "--out", str(out), *map(str, paths)],
                text=True, capture_output=True, check=False,
            )
            if result.returncode == 0:
                result.payload = json.loads(out.read_text())  # type: ignore[attr-defined]
            return result

    def test_envelope_keeps_the_worst_p50_mean_and_rss(self):
        r1 = report([query(1, "upstream", 1_000_000), query(19, "upstream", 120_000_000)], peak_rss=3_000_000_000)
        r2 = report([query(1, "upstream", 2_000_000), query(19, "upstream", 130_000_000)], peak_rss=3_400_000_000)
        result = self.run_freeze([r1, r2])
        self.assertEqual(result.returncode, 0, result.stderr)
        q1 = next(q for q in result.payload["queries"] if q["qid"] == 1)
        q19 = next(q for q in result.payload["queries"] if q["qid"] == 19)
        self.assertEqual(q1["p50_ms"], 2.0)
        self.assertEqual(q19["p50_ms"], 130.0)
        self.assertEqual(result.payload["peak_rss_bytes"], 3_400_000_000)

    def test_envelope_refuses_mixed_environment_classes(self):
        other = dict(ENV, runner="local")
        result = self.run_freeze([
            report([query(1, "upstream", 1_000_000)]),
            report([query(1, "upstream", 1_000_000)], environment=other),
        ])
        self.assertEqual(result.returncode, 1)
        self.assertIn("runner: 'github-actions:ubuntu-24.04' != 'local'", result.stderr)

    def test_envelope_refuses_a_changed_query_set(self):
        result = self.run_freeze([
            report([query(1, "upstream", 1_000_000)]),
            report([query(2, "upstream", 1_000_000)]),
        ])
        self.assertEqual(result.returncode, 1)
        self.assertIn("query set differs", result.stderr)

    def test_envelope_refuses_a_missing_environment(self):
        result = self.run_freeze([report([query(1, "upstream", 1)], environment=None)])
        self.assertEqual(result.returncode, 1)
        self.assertIn("missing environment", result.stderr)


class QueryLatencyCompareTests(unittest.TestCase):
    def base(self):
        return {
            "schema_version": 1,
            "environment": ENV,
            "peak_rss_bytes": 3_000_000_000,
            "queries": [
                {"qid": 1, "index_profile": "upstream", "p50_ms": 0.17, "mean_ms": 0.2},
                {"qid": 19, "index_profile": "upstream", "p50_ms": 130.0, "mean_ms": 140.0},
            ],
        }

    def test_ok_within_bands(self):
        rep = report([query(1, "upstream", 200_000), query(19, "upstream", 190_000_000)])
        status, diags = ql_compare.verdict(self.base(), rep)
        self.assertEqual((status, diags), ("ok", []))

    def test_the_ten_x_regression_of_a_sub_millisecond_query_fails(self):
        # 0.17 ms -> 1.7 ms: the exact shape ADR-0245's 0.5 ms floor is
        # calibrated to catch (a millisecond-scale allowance would not).
        rep = report([query(1, "upstream", 1_700_000), query(19, "upstream", 130_000_000)])
        status, diags = ql_compare.verdict(self.base(), rep)
        self.assertEqual(status, "fail")
        self.assertTrue(any("q1/upstream p50" in d for d in diags))

    def test_band_math_is_max_of_ratio_and_floor(self):
        # Envelope 130 ms: 3.0x = 390 ms beats 130 + 0.5, so 380 ms passes
        # and 400 ms fails (the ratio widened 1.5 -> 3.0 for matrix
        # contention; ADR-0245 later reference). Envelope 0.17 ms: the
        # floor gives 0.67 ms.
        rep = report([query(1, "upstream", 200_000), query(19, "upstream", 400_000_000)])
        status, diags = ql_compare.verdict(self.base(), rep)
        self.assertEqual(status, "fail")
        self.assertTrue(any("q19/upstream p50" in d for d in diags))
        rep = report([query(1, "upstream", 200_000), query(19, "upstream", 380_000_000)])
        status, diags = ql_compare.verdict(self.base(), rep)
        self.assertEqual(status, "ok", diags)

    def test_memory_band_fails_over_115_percent(self):
        rep = report(
            [query(1, "upstream", 200_000), query(19, "upstream", 130_000_000)],
            peak_rss=3_500_000_000,
        )
        status, diags = ql_compare.verdict(self.base(), rep)
        self.assertEqual(status, "fail")
        self.assertTrue(any("peak RSS" in d for d in diags))

    def test_answers_are_hard_failures_independent_of_class(self):
        rep = report([query(1, "upstream", 100_000, status="wrong", detail="answer differs")])
        status, diags = ql_compare.verdict(self.base(), rep)
        self.assertEqual(status, "fail")
        self.assertTrue(any("q1/upstream not ok" in d for d in diags))

    def test_a_missing_query_fails(self):
        rep = report([query(1, "upstream", 100_000)])
        status, diags = ql_compare.verdict(self.base(), rep)
        self.assertEqual(status, "fail")
        self.assertTrue(any("q19/upstream missing from the report" in d for d in diags))

    def test_class_mismatch_is_incompatible_bands_skipped_answers_kept(self):
        local = report(
            [query(1, "upstream", 1_700_000), query(19, "upstream", 130_000_000)],
            environment=dict(ENV, runner="local"),
        )
        status, diags = ql_compare.verdict(self.base(), local)
        self.assertEqual(status, "incompatible")
        self.assertEqual(diags, [])
        wrong = report(
            [query(1, "upstream", 1_700_000, status="wrong", detail="answer differs")],
            environment=dict(ENV, runner="local"),
        )
        status, diags = ql_compare.verdict(self.base(), wrong)
        self.assertEqual(status, "fail")
        self.assertTrue(any("q1/upstream not ok" in d for d in diags))

    def run_compare(self, baseline, rep, extra_env=None):
        """The gate's subprocess run, through the shared comparator
        primitive: only the pair spelling differs from `run_pair` (this
        CLI takes --baseline/--report)."""
        result, _ = run_comparator(
            COMPARE,
            baseline,
            rep,
            lambda base, report_path: [
                "--baseline",
                str(base),
                "--report",
                str(report_path),
            ],
            extra_env=extra_env,
        )
        return result

    def test_subprocess_exit_codes(self):
        ok = report([query(1, "upstream", 200_000), query(19, "upstream", 130_000_000)])
        self.assertEqual(self.run_compare(self.base(), ok).returncode, 0)
        bad = report([query(1, "upstream", 1_700_000), query(19, "upstream", 130_000_000)])
        self.assertEqual(self.run_compare(self.base(), bad).returncode, 1)
        local = report(
            [query(1, "upstream", 1_700_000), query(19, "upstream", 130_000_000)],
            environment=dict(ENV, runner="local"),
        )
        # The class mismatch skips bands locally but hard-fails in
        # GitHub Actions (the loadtest compare's GITHUB_ACTIONS idiom).
        self.assertEqual(self.run_compare(self.base(), local).returncode, 0)
        self.assertEqual(
            self.run_compare(self.base(), local, extra_env={"GITHUB_ACTIONS": "true"}).returncode, 1
        )


if __name__ == "__main__":
    unittest.main()
