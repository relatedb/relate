"""Verdict-layer tests for scripts/compare-loadtest.py.

Pins the shared-verdict refactor: one per-use-case p99 verdict drives both
the gate's die lines and the rendered detail statuses; the failure-count
line reports the true count (an errors-plus-mismatch pair used to
underreport as 1); report-derived free text is HTML-escaped; and render
mode's --status-out / --report-kind flags behave as documented in
docs/datasets.md.
"""

import unittest

from comparator_runner import render, render_mode, run_pair
from loadtest_fixtures import ENV, checkpoint_report, memory_report, report


def uc(uid, ops=100, errors=0, p99=1_000_000):
    return {"id": uid, "ops": ops, "errors": errors, "latency_ns": {"p99": p99}}


class VerdictPinningTests(unittest.TestCase):
    """One verdict drives the gate's die lines and the rendered statuses."""

    def test_breach_usecase_fails_details_and_dies_the_gate(self) -> None:
        base = report(p99=1_000_000)
        cur = report(p99=1_300_000)
        result, table = render(base, cur)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            "| p99 (worst: query) | 1.00 ms | 1.30 ms | +0.30 ms (+30.0%) "
            "| fail (above limit) |",
            table,
        )
        self.assertIn(
            "| query | 100 | 1.00 ms | 1.30 ms | +0.30 ms (+30.0%) | fail (above limit) |",
            table,
        )
        gate, _ = run_pair(base, cur)
        self.assertEqual(gate.returncode, 1)
        self.assertIn(
            "usecase query p99 1300000 > max(1.15 * baseline 1000000, "
            "baseline + 250000ns) (ops=100)",
            gate.stderr,
        )
        self.assertIn("1 check(s) failed", gate.stderr)

    def test_not_gated_usecase_is_skipped_by_gate_and_rendered_as_such(self) -> None:
        base = report(p99=1_000_000, usecases=[uc("b", p99=2_000_000)])
        cur = report(p99=1_000_000, usecases=[uc("b", ops=2, p99=5_000_000)])
        result, table = render(base, cur)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            "| b | 2 | 2.00 ms | 5.00 ms | +3.00 ms (+150.0%) | not gated (ops < 8) |",
            table,
        )
        self.assertIn("| p99 | — | — | — | n/a (no comparable use cases) |", table)
        gate, _ = run_pair(base, cur)
        self.assertEqual(gate.returncode, 0, gate.stderr)
        self.assertIn("compare-loadtest: ok", gate.stdout)

    def test_missing_usecase_fails_details_and_dies_the_gate(self) -> None:
        base = report(p99=1_000_000, usecases=[uc("a"), uc("b", p99=2_000_000)])
        cur = report(p99=1_000_000, usecases=[uc("a")])
        result, table = render(base, cur)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("| b | — | 2.00 ms | — | — | fail (missing in PR) |", table)
        gate, _ = run_pair(base, cur)
        self.assertEqual(gate.returncode, 1)
        self.assertIn("missing usecase b", gate.stderr)

    def test_zero_p99_renders_as_zero_not_missing(self) -> None:
        # the unified formatter family: _fmt_ms tests `is None`, so a
        # measured 0 ns is a value, not a missing cell
        result, table = render(report(p99=1_000_000), report(p99=0))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            "| query | 100 | 1.00 ms | 0.00 ms | -1.00 ms (-100.0%) | n/a (no p99) |",
            table,
        )


class FailureCountTests(unittest.TestCase):
    """The exit tail reports the true count of failed checks, including the
    environment mismatch itself (previously hardcoded 1 in CI and excluded
    from the local count)."""

    def test_errors_plus_mismatch_counts_both_checks(self) -> None:
        other = dict(ENV, runner="local")
        base = report(p99=1_000_000)
        cur = report(p99=1_000_000, env=other, errors=1)
        local, _ = run_pair(base, cur)
        self.assertEqual(local.returncode, 1)
        self.assertIn("errors=1", local.stderr)
        self.assertIn("INCOMPATIBLE", local.stderr)
        self.assertIn("skipping ops/s and p99 (environment class mismatch)", local.stderr)
        self.assertIn("2 check(s) failed", local.stderr)
        ci, _ = run_pair(base, cur, extra_env={"GITHUB_ACTIONS": "true"})
        self.assertEqual(ci.returncode, 1)
        self.assertIn("2 check(s) failed", ci.stderr)

    def test_memory_shape_logical_plus_mismatch_counts_both_checks(self) -> None:
        other = dict(ENV, runner="local")
        base = memory_report(logical=1000, ratio=2.0)
        cur = memory_report(logical=1001, ratio=2.0, env=other)
        local, _ = run_pair(base, cur)
        self.assertEqual(local.returncode, 1)
        self.assertIn("logical_bytes", local.stderr)
        self.assertIn("skipping rss ratio (environment class mismatch)", local.stderr)
        self.assertIn("2 check(s) failed", local.stderr)
        ci, _ = run_pair(base, cur, extra_env={"GITHUB_ACTIONS": "true"})
        self.assertEqual(ci.returncode, 1)
        self.assertIn("2 check(s) failed", ci.stderr)

    def test_pure_mismatch_in_ci_still_counts_exactly_one(self) -> None:
        other = dict(ENV, runner="local")
        result, _ = run_pair(
            report(p99=1_000_000),
            report(p99=1_000_000, env=other, ops_per_sec=1.0),
            extra_env={"GITHUB_ACTIONS": "true"},
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("1 check(s) failed", result.stderr)

    def test_schema_and_errors_plus_mismatch_counts_three(self) -> None:
        other = dict(ENV, runner="local")
        base = report(p99=1_000_000, schema=5)
        cur = report(p99=1_000_000, schema=6, env=other, errors=1)
        result, _ = run_pair(base, cur)
        self.assertEqual(result.returncode, 1)
        self.assertIn("3 check(s) failed", result.stderr)

    def test_plain_band_breach_counts_stay_unchanged(self) -> None:
        result, _ = run_pair(
            report(p99=1_000_000),
            report(p99=1_300_000, ops_per_sec=50.0, logical_used_bytes=2000),
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("3 check(s) failed", result.stderr)


class EscapingTests(unittest.TestCase):
    """Report-derived free text is HTML-escaped: a hostile use-case id or
    environment value cannot close the <details> block or inject markup."""

    HOSTILE_ID = "</details><img src=x onerror=alert(1)>"

    def test_hostile_usecase_id_is_escaped(self) -> None:
        usecases = [uc(self.HOSTILE_ID, p99=1_000_000)]
        base = report(p99=1_000_000, usecases=usecases)
        cur = report(p99=1_100_000, usecases=[uc(self.HOSTILE_ID, p99=1_100_000)])
        result, table = render(base, cur)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn(self.HOSTILE_ID, table)
        self.assertNotIn("<img", table)
        self.assertIn("&lt;/details&gt;&lt;img src=x onerror=alert(1)&gt;", table)
        # the injected id cannot close the genuine details block early
        self.assertEqual(table.count("<details>"), 1)
        self.assertEqual(table.count("</details>"), 1)
        # the worst-p99 label escapes the same id
        self.assertIn("p99 (worst: &lt;/details&gt;", table)

    def test_pipe_in_usecase_id_is_escaped_once(self) -> None:
        usecases = [uc("a|b")]
        base = report(p99=1_000_000, usecases=usecases)
        cur = report(p99=1_100_000, usecases=[uc("a|b", p99=1_100_000)])
        result, table = render(base, cur)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("| a\\|b | 100 | 1.00 ms | 1.10 ms |", table)

    def test_hostile_environment_values_and_class_are_escaped(self) -> None:
        other = dict(ENV, runner="</script><b>local</b>")
        other["class"] = "<i>cls</i>"
        result, table = render(report(p99=1_000_000), report(p99=1_000_000, env=other))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("<b>", table)
        self.assertNotIn("<i>", table)
        self.assertIn("&lt;b&gt;local&lt;/b&gt;", table)  # mismatch line
        self.assertIn("&lt;i&gt;cls&lt;/i&gt;", table)  # environment row class cells

    def test_hostile_schema_and_kind_values_are_escaped(self) -> None:
        base = report(p99=1_000_000, schema="<s>5</s>", kind="<k>identity</k>")
        cur = report(p99=1_000_000, schema=6, kind="fraud")
        result, table = render(base, cur)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("<s>", table)
        self.assertNotIn("<k>", table)
        self.assertIn("&lt;s&gt;5&lt;/s&gt;", table)
        self.assertIn("&lt;k&gt;identity&lt;/k&gt;", table)

    def test_hostile_shape_and_index_labels_are_escaped(self) -> None:
        def hostile_memory():
            return {
                "schema_version": 1,
                "workload": {"kind": "memory-shape"},
                "environment": ENV,
                "shapes": [
                    {"shape": "<x>", "index": "&y", "logical_bytes": 5, "ratio": 1.5}
                ],
            }

        result, table = render(hostile_memory(), hostile_memory())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("<x>", table)
        self.assertIn("logical bytes — &lt;x&gt;/&amp;y", table)
        self.assertIn("rss ratio — &lt;x&gt;/&amp;y", table)


class StatusOutTests(unittest.TestCase):
    """--status-out writes one machine-readable token per rendered pair."""

    def status(self, baseline, current, flags=(), extra_env=None):
        result, written = render_mode(
            baseline, current, flags, sidecars=("--status-out",), extra_env=extra_env
        )
        return result, written["--status-out"]

    def test_ok_pair_writes_ok(self) -> None:
        result, token = self.status(
            report(p99=1_000_000), report(p99=1_100_000, ops_per_sec=95.0)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(token, "ok\n")

    def test_failing_rows_outranks_environment_mismatch(self) -> None:
        other = dict(ENV, runner="local")
        result, token = self.status(
            report(p99=1_000_000), report(p99=1_000_000, env=other, errors=1)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(token, "failing-rows\n")

    def test_band_breach_writes_failing_rows(self) -> None:
        result, token = self.status(
            report(p99=1_000_000), report(p99=1_300_000, ops_per_sec=50.0)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(token, "failing-rows\n")

    def test_mismatch_without_failing_rows_writes_environment_mismatch(self) -> None:
        other = dict(ENV, runner="local")
        result, token = self.status(
            report(p99=1_000_000),
            report(p99=1_000_000, env=other, ops_per_sec=1.0),
            extra_env={"GITHUB_ACTIONS": "true"},
        )
        self.assertEqual(result.returncode, 0, result.stderr)  # render never gates
        self.assertEqual(token, "environment-mismatch\n")

    def test_schema_mismatch_writes_not_comparable(self) -> None:
        result, token = self.status(
            report(p99=1_000_000, schema=5), report(p99=1_000_000, schema=6)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(token, "not-comparable\n")

    def test_status_out_is_inert_in_gate_mode(self) -> None:
        base = report(p99=1_000_000)
        cur = report(p99=1_300_000, ops_per_sec=50.0)
        plain, _ = run_pair(base, cur)
        flagged, written = run_pair(
            base, cur, ("--report-kind", "generic"), sidecars=("--status-out",)
        )
        self.assertIsNone(written["--status-out"])  # never written without --render-only
        self.assertEqual(flagged.returncode, plain.returncode)
        self.assertEqual(flagged.stdout, plain.stdout)
        self.assertEqual(flagged.stderr, plain.stderr)


class ReportKindTests(unittest.TestCase):
    """--report-kind dispatches the table explicitly; without it the kind is
    sniffed from the report contents."""

    def test_explicit_checkpoint_kind_renders_checkpoint_rows(self) -> None:
        base = checkpoint_report()
        cur = checkpoint_report(logical=7_500_000)
        result, table = render(base, cur, flags=("--report-kind", "checkpoint"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            "| logical_used_bytes | 7,211,831 | 7,500,000 | +288,169 (+4.0%) | info |",
            table,
        )

    def test_generic_kind_renders_generic_rows_for_checkpoint_reports(self) -> None:
        base = checkpoint_report()
        cur = checkpoint_report(logical=7_500_000)
        result, table = render(base, cur, flags=("--report-kind", "generic"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("| errors | 0 | 0 | +0 | ok |", table)
        self.assertIn("| logical bytes | — | — | — | n/a |", table)
        self.assertNotIn("| logical_used_bytes |", table)

    def test_memory_shape_kind_dispatches_without_sniffing(self) -> None:
        # a fraud-kind pair rendered as memory-shape: no shapes, so the
        # table carries only the environment row — the flag wins
        result, table = render(
            report(p99=1_000_000),
            report(p99=1_100_000),
            flags=("--report-kind", "memory-shape"),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            "| environment | relate-loadtest-baseline/v1 "
            "| relate-loadtest-baseline/v1 | — | match |",
            table,
        )
        self.assertNotIn("| errors |", table)

    def test_sniff_fallback_renders_checkpoint_and_memory_pairs(self) -> None:
        result, table = render(checkpoint_report(), checkpoint_report(logical=7_500_000))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("| logical_used_bytes |", table)  # checkpoint shape sniffed
        result, table = render(
            memory_report(logical=1000, ratio=2.0), memory_report(logical=1000, ratio=2.2)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("| rss ratio — deep/none | 2.000 | 2.200 | +0.200 (+10.0%) | ok |", table)

    def test_unknown_report_kind_is_rejected(self) -> None:
        result, _ = run_pair(
            report(p99=1_000_000), report(p99=1_000_000), ("--report-kind", "bogus")
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("invalid choice", result.stderr)

    def test_report_kind_does_not_change_the_gate(self) -> None:
        # gate mode dispatches on workload.kind itself; the flag is inert
        base = memory_report(logical=1000, ratio=2.0)
        cur = memory_report(logical=1001, ratio=2.0)
        plain, _ = run_pair(base, cur)
        flagged, _ = run_pair(base, cur, ("--report-kind", "generic"))
        self.assertEqual(flagged.returncode, plain.returncode)
        self.assertIn("logical_bytes", flagged.stderr)  # still the memory-shape gate
        self.assertEqual(flagged.stderr, plain.stderr)


if __name__ == "__main__":
    unittest.main()
