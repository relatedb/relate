import subprocess
import unittest

from comparator_runner import render, run_pair
from loadtest_fixtures import ENV, checkpoint_report, memory_report, report


class CompareLoadtestTests(unittest.TestCase):
    def compare(
        self,
        baseline: dict,
        current: dict,
        extra_env: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        result, _ = run_pair(baseline, current, extra_env=extra_env)
        return result

    def test_allows_submillisecond_p99_jitter(self) -> None:
        result = self.compare(report(p99=100_000), report(p99=340_000))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_p99_beyond_ratio_and_jitter(self) -> None:
        result = self.compare(report(p99=1_000_000), report(p99=1_300_000))
        self.assertEqual(result.returncode, 1)
        self.assertIn("p99", result.stderr)

    def test_still_rejects_current_errors(self) -> None:
        result = self.compare(report(p99=1_000_000), report(p99=1_000_000, errors=1))
        self.assertEqual(result.returncode, 1)
        self.assertIn("errors=1", result.stderr)

    def test_rejects_schema_version_mismatch(self) -> None:
        result = self.compare(report(p99=1_000_000, schema=5), report(p99=1_000_000, schema=6))
        self.assertEqual(result.returncode, 1)
        self.assertIn("schema_version", result.stderr)

    def test_rejects_workload_kind_mismatch(self) -> None:
        result = self.compare(
            report(p99=1_000_000, kind="identity"), report(p99=1_000_000, kind="fraud")
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("workload.kind", result.stderr)

    def test_reports_without_schema_still_compare(self) -> None:
        result = self.compare(
            report(p99=1_000_000, schema=None, kind=None),
            report(p99=1_000_000, schema=None, kind=None),
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_ci_hard_fails_environment_mismatch(self) -> None:
        other = dict(ENV, runner="local")
        result = self.compare(
            report(p99=1_000_000),
            report(p99=1_000_000, env=other, ops_per_sec=1.0),
            extra_env={"GITHUB_ACTIONS": "true"},
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("INCOMPATIBLE", result.stderr)
        self.assertNotIn("ops_per_sec", result.stderr)

    def test_local_skips_perf_on_mismatch_but_keeps_errors(self) -> None:
        other = dict(ENV, runner="local")
        ok = self.compare(
            report(p99=1_000_000),
            report(p99=50_000_000, env=other, ops_per_sec=1.0),
        )
        self.assertEqual(ok.returncode, 0, ok.stderr)
        self.assertIn("INCOMPATIBLE", ok.stderr)
        self.assertIn("ok (correctness only)", ok.stdout)

        bad = self.compare(
            report(p99=1_000_000),
            report(p99=1_000_000, env=other, errors=1),
        )
        self.assertEqual(bad.returncode, 1)
        self.assertIn("errors=1", bad.stderr)

    def test_missing_environment_is_incompatible(self) -> None:
        result = self.compare(
            report(p99=1_000_000, env=None),
            report(p99=1_000_000, env=None),
            extra_env={"GITHUB_ACTIONS": "true"},
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("INCOMPATIBLE", result.stderr)
        self.assertIn("missing environment", result.stderr)

    def test_matched_class_still_rejects_throughput(self) -> None:
        result = self.compare(
            report(p99=1_000_000, ops_per_sec=100.0),
            report(p99=1_000_000, ops_per_sec=50.0),
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("ops_per_sec", result.stderr)

    def test_memory_shape_pins_logical_bytes_and_bands_rss_ratio(self) -> None:
        base = memory_report(logical=1000, ratio=2.0)
        ok = self.compare(base, memory_report(logical=1000, ratio=2.2))
        self.assertEqual(ok.returncode, 0, ok.stderr)
        logical = self.compare(base, memory_report(logical=1001, ratio=2.0))
        self.assertEqual(logical.returncode, 1)
        self.assertIn("logical_bytes", logical.stderr)
        ratio_breach = self.compare(base, memory_report(logical=1000, ratio=2.4))
        self.assertEqual(ratio_breach.returncode, 1)
        self.assertIn("rss ratio", ratio_breach.stderr)

    def test_memory_shape_skips_ratio_on_environment_mismatch(self) -> None:
        other = dict(ENV, runner="local")
        result = self.compare(
            memory_report(logical=1000, ratio=2.0),
            memory_report(logical=1000, ratio=9.0, env=other),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("skipping rss ratio", result.stderr)
        logical = self.compare(
            memory_report(logical=1000, ratio=2.0),
            memory_report(logical=1001, ratio=2.0, env=other),
        )
        self.assertEqual(logical.returncode, 1)
        self.assertIn("logical_bytes", logical.stderr)


class RenderMarkdownTests(unittest.TestCase):
    """--render-only / --markdown-out render tables without touching the gate."""

    def test_matched_pair_renders_all_rows_and_details_block(self) -> None:
        base = report(p99=1_000_000, ops_per_sec=100.0, logical_used_bytes=1000)
        cur = report(p99=1_100_000, ops_per_sec=95.0, logical_used_bytes=1050)
        result, table = render(base, cur)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertEqual(result.stdout, "")
        self.assertIn("### base", table)
        self.assertIn("| Metric | Baseline | PR | Δ | Status |", table)
        self.assertIn(
            "| environment | relate-loadtest-baseline/v1 "
            "| relate-loadtest-baseline/v1 | — | match |",
            table,
        )
        self.assertIn("| errors | 0 | 0 | +0 | ok |", table)
        self.assertIn("| ops/s | 100.00 | 95.00 | -5.00 (-5.0%) | ok |", table)
        self.assertIn(
            "| p99 (worst: query) | 1.00 ms | 1.10 ms | +0.10 ms (+10.0%) | ok |", table
        )
        self.assertIn("| logical bytes | 1,000 | 1,050 | +50 (+5.0%) | ok |", table)
        # per-usecase p99 breakdown collapsed behind a details summary
        self.assertIn("<details>", table)
        self.assertIn("<summary>Per-use-case p99 latency</summary>", table)
        self.assertIn("| query | 100 | 1.00 ms | 1.10 ms | +0.10 ms (+10.0%) | ok |", table)
        self.assertIn("</details>", table)

    def test_render_only_exits_zero_when_metrics_are_out_of_band(self) -> None:
        base = report(p99=1_000_000)
        cur = report(p99=1_300_000, ops_per_sec=50.0, logical_used_bytes=2000, errors=1)
        result, table = render(base, cur)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")  # no FAIL/INCOMPATIBLE gate lines
        self.assertIn("| errors | 0 | 1 | +1 | fail (1 errors) |", table)
        self.assertIn(
            "| ops/s | 100.00 | 50.00 | -50.00 (-50.0%) | fail (< 0.9× baseline) |", table
        )
        self.assertIn(
            "| p99 (worst: query) | 1.00 ms | 1.30 ms | +0.30 ms (+30.0%) "
            "| fail (above limit) |",
            table,
        )
        self.assertIn(
            "| logical bytes | 1,000 | 2,000 | +1,000 (+100.0%) | fail (> 1.1× baseline) |",
            table,
        )
        # the same pair still fails the default gate
        gate, _ = run_pair(base, cur)
        self.assertEqual(gate.returncode, 1)
        self.assertIn("ops_per_sec", gate.stderr)

    def test_render_only_skips_ops_and_p99_on_environment_mismatch(self) -> None:
        other = dict(ENV, runner="local")
        base = report(p99=1_000_000)
        cur = report(p99=1_000_000, env=other, ops_per_sec=1.0, logical_used_bytes=1001)
        # render-only exits 0 even inside GitHub Actions
        result, table = render(base, cur, extra_env={"GITHUB_ACTIONS": "true"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertIn(
            "| environment | relate-loadtest-baseline/v1 "
            "| relate-loadtest-baseline/v1 | — | mismatch (runner) |",
            table,
        )
        self.assertIn("> **Environment class mismatch (INCOMPATIBLE):**", table)
        self.assertIn("| ops/s | 100.00 | 1.00 | — | skipped (INCOMPATIBLE) |", table)
        self.assertIn(
            "| p99 (worst: query) | 1.00 ms | 1.00 ms | — | skipped (INCOMPATIBLE) |", table
        )
        # logical bytes stay banded (correctness-only, ADR-0071)
        self.assertIn("| logical bytes | 1,000 | 1,001 | +1 (+0.1%) | ok |", table)
        # details rows are marked skipped, not evaluated
        self.assertIn("| query | 100 | 1.00 ms | 1.00 ms | — | skipped |", table)

    def test_mismatch_renders_explainer_line_render_mode_only(self) -> None:
        other = dict(ENV, runner="local")
        base = report(p99=1_000_000)
        cur = report(p99=1_000_000, env=other, ops_per_sec=1.0)
        result, table = render(base, cur)
        self.assertEqual(result.returncode, 0, result.stderr)
        # the explainer sits directly under the mismatch blockquote
        self.assertIn(
            "> **Environment class mismatch (INCOMPATIBLE):** "
            "runner: 'github-actions:ubuntu-24.04' != 'local'\n"
            "> Wall-clock metrics (ops/s, p99) are skipped for this pair: "
            "the environment class changed since the baseline was frozen, so "
            "the runs are not comparable (ADR-0071). Re-freeze via the "
            "create-loadtest-baselines workflow to restore comparison.\n",
            table,
        )
        # benign pairs never carry it
        result, table = render(report(p99=1_000_000), report(p99=1_000_000))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("Wall-clock metrics", table)
        self.assertNotIn("Re-freeze", table)
        # gate mode never emits it, locally or in GitHub Actions
        local, _ = run_pair(base, cur)
        self.assertEqual(local.returncode, 0, local.stderr)
        self.assertNotIn("Wall-clock metrics", local.stdout + local.stderr)
        ci, _ = run_pair(base, cur, extra_env={"GITHUB_ACTIONS": "true"})
        self.assertEqual(ci.returncode, 1)
        self.assertNotIn("Wall-clock metrics", ci.stdout + ci.stderr)

    def test_mismatch_explainer_names_the_pair_kind_s_devalued_rows(self) -> None:
        other = dict(ENV, runner="local")
        result, table = render(
            memory_report(logical=1000, ratio=2.0),
            memory_report(logical=1000, ratio=9.0, env=other),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            "> RSS-ratio rows are skipped for this pair: the environment "
            "class changed since the baseline was frozen, so the runs are not "
            "comparable (ADR-0071). Re-freeze via the create-loadtest-baselines "
            "workflow to restore comparison.",
            table,
        )
        self.assertNotIn("Wall-clock metrics", table)
        result, table = render(
            checkpoint_report(), checkpoint_report(env=other)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            "> Size rows are informational only: the environment class "
            "changed since the baseline was frozen, so the runs are not "
            "comparable (ADR-0071). Re-freeze via the create-loadtest-baselines "
            "workflow to restore comparison.",
            table,
        )

    def test_render_only_memory_shape_rows(self) -> None:
        base = memory_report(logical=1000, ratio=2.0)
        cur = memory_report(logical=1000, ratio=2.2)
        result, table = render(base, cur)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("| environment | relate-loadtest-baseline/v1 "
                      "| relate-loadtest-baseline/v1 | — | match |", table)
        self.assertIn("| logical bytes — baseline/none | 0 | 0 | +0 | ok |", table)
        self.assertIn("| rss ratio — baseline/none | — | — | — | n/a |", table)
        self.assertIn("| logical bytes — deep/none | 1,000 | 1,000 | +0 (+0.0%) | ok |", table)
        self.assertIn("| rss ratio — deep/none | 2.000 | 2.200 | +0.200 (+10.0%) | ok |", table)

    def test_render_only_memory_shape_breaches_and_env_skip(self) -> None:
        base = memory_report(logical=1000, ratio=2.0)
        cur = memory_report(logical=1001, ratio=2.4)
        result, table = render(base, cur)
        self.assertEqual(result.returncode, 0, result.stderr)  # breaches still render, exit 0
        self.assertIn(
            "| logical bytes — deep/none | 1,000 | 1,001 | +1 (+0.1%) | fail (must be exact) |",
            table,
        )
        self.assertIn(
            "| rss ratio — deep/none | 2.000 | 2.400 | +0.400 (+20.0%) "
            "| fail (> 1.15× baseline) |",
            table,
        )
        # environment mismatch: logical bytes stay exact, rss ratios skip
        other = dict(ENV, runner="local")
        result, table = render(base, memory_report(logical=1000, ratio=9.0, env=other))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            "| rss ratio — deep/none | 2.000 | 9.000 | — | skipped (INCOMPATIBLE) |", table
        )
        self.assertIn("| logical bytes — deep/none | 1,000 | 1,000 | +0 (+0.0%) | ok |", table)

    def test_render_only_checkpoint_rows(self) -> None:
        base = checkpoint_report()
        cur = checkpoint_report(logical=7_500_000)
        result, table = render(base, cur)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("| environment | relate-loadtest-baseline/v1 "
                      "| relate-loadtest-baseline/v1 | — | match |", table)
        self.assertIn(
            "| logical_used_bytes | 7,211,831 | 7,500,000 | +288,169 (+4.0%) | info |", table
        )
        self.assertIn("| rss_bytes | 84,414,464 | 84,414,464 | +0 (+0.0%) | info |", table)
        self.assertIn("| data_file_bytes | 678,934 | 678,934 | +0 (+0.0%) | info |", table)

    def test_render_only_writes_table_to_stdout_without_markdown_out(self) -> None:
        result, _ = run_pair(
            report(p99=1_000_000), report(p99=1_000_000), flags=("--render-only",)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertIn("| Metric | Baseline | PR | Δ | Status |", result.stdout)
        self.assertIn("| environment |", result.stdout)
        self.assertIn("</details>", result.stdout)

    def test_markdown_out_sidecar_leaves_gate_untouched(self) -> None:
        base = report(p99=1_000_000)
        cur = report(p99=1_300_000, ops_per_sec=50.0, logical_used_bytes=2000, errors=1)
        plain, _ = run_pair(base, cur)
        sidecar, written = run_pair(base, cur, sidecars=("--markdown-out",))
        table = written["--markdown-out"]
        self.assertEqual(sidecar.returncode, plain.returncode)
        self.assertEqual(sidecar.stdout, plain.stdout)
        self.assertEqual(sidecar.stderr, plain.stderr)
        self.assertIn(
            "| ops/s | 100.00 | 50.00 | -50.00 (-50.0%) | fail (< 0.9× baseline) |", table
        )
        ok, written = run_pair(
            report(p99=1_000_000), report(p99=1_000_000), sidecars=("--markdown-out",)
        )
        self.assertEqual(ok.returncode, 0, ok.stderr)
        self.assertIn("compare-loadtest: ok", ok.stdout)
        self.assertIn("| errors | 0 | 0 | +0 | ok |", written["--markdown-out"])

    def test_details_block_lists_every_usecase(self) -> None:
        base_ucs = [
            {"id": "a", "ops": 100, "errors": 0, "latency_ns": {"p99": 1_000_000}},
            {"id": "b", "ops": 100, "errors": 0, "latency_ns": {"p99": 2_000_000}},
        ]
        cur_ucs = [
            {"id": "a", "ops": 100, "errors": 0, "latency_ns": {"p99": 1_100_000}},
            # ops below LOADTEST_MIN_OPS_FOR_P99: the gate never evaluates b
            {"id": "b", "ops": 2, "errors": 0, "latency_ns": {"p99": 5_000_000}},
            # new in the PR, not in the baseline
            {"id": "c", "ops": 50, "errors": 0, "latency_ns": {"p99": 3_000_000}},
        ]
        base = report(p99=1_000_000, usecases=base_ucs)
        cur = report(p99=1_000_000, usecases=cur_ucs)
        result, table = render(base, cur)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("| a | 100 | 1.00 ms | 1.10 ms | +0.10 ms (+10.0%) | ok |", table)
        self.assertIn(
            "| b | 2 | 2.00 ms | 5.00 ms | +3.00 ms (+150.0%) | not gated (ops < 8) |", table
        )
        self.assertIn("| c | 50 | — | 3.00 ms | — | new (not in baseline) |", table)
        # top-line p99 reflects only gated use cases: a
        self.assertIn("| p99 (worst: a) | 1.00 ms | 1.10 ms | +0.10 ms (+10.0%) | ok |", table)
        # a baseline use case missing from the PR is called out
        result, table = render(base, report(p99=1_000_000, usecases=base_ucs[:1]))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("| b | — | 2.00 ms | — | — | fail (missing in PR) |", table)


if __name__ == "__main__":
    unittest.main()
