"""Tests for the compare-baselines.sh aggregator.

Render mode (`--render-only`) aggregates every registry pair — fraud and
identity — into ONE markdown document
with a section per pair, exits 0 even when reports are missing, and
appends to $GITHUB_STEP_SUMMARY when set. The pair set is declared once as
the PAIR_REGISTRY both modes iterate: the CI job runs and reports only the
fraud and identity loadtests, so memory-shape and checkpoint are not
members and their baselines/reports are ignored even when present (the
comparator keeps their kinds; its own suite exercises them). Each pair is
classified from the comparator's `--status-out` token (never by grepping
the rendered prose) and the document opens with a producer-owned
`**Status:** ok|warning` line derived from the pair tokens. Gate mode (no
flag) compares the registry's gated members: fraud and identity
unconditionally.
"""

import json
import subprocess
import tempfile
import unittest
from pathlib import Path

from loadtest_fixtures import (
    ENV,
    aggregator_env,
    checkpoint_report,
    memory_report,
    report,
)


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "compare-baselines.sh"


class CompareBaselinesTestBase(unittest.TestCase):
    """Fixture tree (temp baselines/ + reports/) and script runner."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.baselines = Path(self._tmp.name) / "baselines"
        self.reports = Path(self._tmp.name) / "reports"
        self.baselines.mkdir()
        self.reports.mkdir()

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def write_baseline(self, name: str, doc: dict) -> None:
        (self.baselines / name).write_text(json.dumps(doc))

    def write_report(self, name: str, doc: dict) -> None:
        (self.reports / name).write_text(json.dumps(doc))

    def write_stub(self, body: str) -> Path:
        """An executable PYTHON stub: every invocation is the comparator
        call and runs `body`."""
        stub = Path(self._tmp.name) / "stub-compare"
        stub.write_text("#!/usr/bin/env bash\n" + body)
        stub.chmod(0o755)
        return stub

    def default_tree(self) -> None:
        """Today's committed-baseline shape: fraud, identity, and checkpoint
        baselines, no memory-shape baseline. The
        checkpoint baseline is deliberately kept on disk: checkpoint and
        memory-shape are not registry members, so their files must be
        ignored by both modes."""
        self.write_baseline("fraud-loadtest.json", report(p99=1_000_000))
        self.write_baseline(
            "identity-loadtest.json", report(p99=1_000_000, kind="identity")
        )
        self.write_baseline("fraud-checkpoint.json", checkpoint_report())

    def run_script(
        self,
        *args: str,
        summary: Path | None = None,
        extra_env: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        # Render mode must not depend on the Actions reporting pair; gate
        # mode must stay deterministic outside a GitHub Actions runner.
        # The aggregator's env contract is loadtest_fixtures.aggregator_env.
        env = aggregator_env(
            self.baselines,
            self.reports,
            {
                "GITHUB_STEP_SUMMARY": (
                    str(summary) if summary is not None else None
                ),
                **(extra_env or {}),
            },
        )
        return subprocess.run(
            ["bash", str(SCRIPT), *args],
            text=True,
            capture_output=True,
            check=False,
            env=env,
        )


class CompareBaselinesRenderTests(CompareBaselinesTestBase):
    """--render-only: one combined document, per-pair sections, never a gate."""

    def render(
        self, **kw
    ) -> tuple[subprocess.CompletedProcess[str], str]:
        result = self.run_script("--render-only", **kw)
        doc = (self.reports / "loadtest-comparison.md").read_text()
        return result, doc

    def pairs_line(self, doc: str) -> str:
        """The document's `**Pairs:**` summary line — asserted whole, so a
        registry re-add (or reorder) of a non-member pair fails here."""
        return next(
            line for line in doc.splitlines() if line.startswith("**Pairs:**")
        )

    def test_missing_reports_render_notes_and_exit_zero(self) -> None:
        self.default_tree()
        result, doc = self.render()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertEqual(result.stdout, doc)  # printed when no step summary
        self.assertTrue(doc.startswith("# Loadtest baseline comparison\n"))
        self.assertIn("**Status:** warning\n", doc)  # every pair is a note
        self.assertEqual(
            self.pairs_line(doc),
            "**Pairs:** fraud-loadtest: report-missing"
            " · identity-loadtest: report-missing",
        )
        for stem in ("fraud-loadtest", "identity-loadtest"):
            self.assertIn(f"### {stem}\n", doc)
            self.assertIn(f"no current report at `{self.reports}/{stem}.json`", doc)
        # checkpoint and memory-shape left the registry: no section and no
        # note for either, even though a checkpoint baseline is committed
        self.assertNotIn("fraud-checkpoint", doc)
        self.assertNotIn("memory-shape", doc)
        # nothing was comparable, so no table
        self.assertNotIn("| Metric |", doc)

    def test_present_reports_render_per_pair_tables(self) -> None:
        self.default_tree()
        self.write_report(
            "fraud-loadtest.json",
            report(p99=1_100_000, ops_per_sec=95.0, logical_used_bytes=1050),
        )
        self.write_report(
            "identity-loadtest.json", report(p99=1_000_000, kind="identity")
        )
        # stray sides for pairs that left the registry: a frozen
        # memory-shape baseline+report and a checkpoint report must not
        # render — membership is the registry's, never the tree's
        self.write_baseline(
            "memory-shape-loadtest.json", memory_report(logical=1000, ratio=2.0)
        )
        self.write_report(
            "memory-shape-loadtest.json", memory_report(logical=1000, ratio=2.2)
        )
        self.write_report("fraud-checkpoint.json", checkpoint_report(logical=7_500_000))
        result, doc = self.render()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertIn("**Status:** ok\n", doc)  # every comparable pair is clean
        self.assertEqual(
            self.pairs_line(doc),
            "**Pairs:** fraud-loadtest: compared · identity-loadtest: compared",
        )
        # fraud carries wave 0's per-pair table, verbatim
        self.assertIn(
            f"`{self.baselines}/fraud-loadtest.json` → "
            f"`{self.reports}/fraud-loadtest.json`",
            doc,
        )
        self.assertIn("| Metric | Baseline | PR | Δ | Status |", doc)
        self.assertIn("| ops/s | 100.00 | 95.00 | -5.00 (-5.0%) | ok |", doc)
        self.assertIn("<summary>Per-use-case p99 latency</summary>", doc)
        # the non-member pairs render neither a section nor a note
        self.assertNotIn("fraud-checkpoint", doc)
        self.assertNotIn("memory-shape", doc)

    def test_failing_rows_and_environment_mismatch_statuses(self) -> None:
        self.default_tree()
        self.write_report(
            "fraud-loadtest.json",
            report(p99=1_300_000, ops_per_sec=50.0, logical_used_bytes=2000, errors=1),
        )
        other = dict(ENV, runner="local")
        self.write_report(
            "identity-loadtest.json",
            report(p99=1_000_000, kind="identity", env=other),
        )
        # render mode exits 0 even inside GitHub Actions with out-of-band metrics
        result, doc = self.render(extra_env={"GITHUB_ACTIONS": "true"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertIn("**Pairs:** fraud-loadtest: compared (failing rows)", doc)
        self.assertIn("identity-loadtest: compared (environment mismatch)", doc)
        self.assertIn("**Status:** warning\n", doc)
        self.assertIn("| errors | 0 | 1 | +1 | fail (1 errors) |", doc)
        self.assertIn("> **Environment class mismatch (INCOMPATIBLE):**", doc)
        self.assertIn(
            "| ops/s | 100.00 | 100.00 | — | skipped (INCOMPATIBLE) |", doc
        )

    def test_document_appends_to_step_summary_when_set(self) -> None:
        self.default_tree()
        summary = Path(self._tmp.name) / "step-summary.md"
        summary.write_text("prior step summary\n")
        out = Path(self._tmp.name) / "comparison-out.md"
        result = self.run_script(
            "--render-only", summary=summary, extra_env={"COMPARISON_OUT": str(out)}
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")  # appended, not printed
        appended = summary.read_text()
        self.assertTrue(appended.startswith("prior step summary\n\n"))
        self.assertTrue(appended.endswith(out.read_text()))
        self.assertIn("### identity-loadtest\n", appended)
        self.assertIn("**Status:** warning\n", appended)  # all-notes document
        # COMPARISON_OUT overrides the default document path
        self.assertFalse((self.reports / "loadtest-comparison.md").exists())

    def test_corrupt_report_renders_error_note_and_exits_zero(self) -> None:
        self.default_tree()
        (self.reports / "fraud-loadtest.json").write_text("{not json")
        result, doc = self.render()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("fraud-loadtest: render-error", doc)
        self.assertIn("> **Warning (render-error):** comparator exited", doc)
        self.assertIn("**Status:** warning\n", doc)
        self.assertIn(
            "compare-baselines: render failed for fraud-loadtest.json", result.stderr
        )
        # the other pairs still render
        self.assertIn("identity-loadtest: report-missing", doc)

    def test_schema_mismatch_pairs_render_not_comparable_token(self) -> None:
        # comparator token `not-comparable` (schema_version differs) maps to
        # its own **Pairs:** token; the old prose grep silently read this as
        # a plain `compared`
        self.default_tree()
        self.write_baseline("fraud-loadtest.json", report(p99=1_000_000, schema=5))
        self.write_report("fraud-loadtest.json", report(p99=1_000_000))
        result, doc = self.render()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertIn("**Pairs:** fraud-loadtest: compared (not comparable)", doc)
        self.assertIn("schema_version 6 != baseline 5", doc)
        self.assertIn("**Status:** warning\n", doc)

    def test_render_error_note_neutralizes_raw_stderr(self) -> None:
        # comparator stderr is not authored by us: no unescaped & < > may
        # reach the markdown note
        self.default_tree()
        for name in ("fraud-loadtest.json", "identity-loadtest.json"):
            self.write_report(name, report(p99=1_000_000))
        stub = self.write_stub('echo "boom <script> & </details>" >&2\nexit 1\n')
        result, doc = self.render(extra_env={"PYTHON": str(stub)})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("fraud-loadtest: render-error", doc)
        self.assertIn(
            "comparator exited 1: boom &lt;script&gt; &amp; &lt;/details&gt;",
            doc,
        )
        self.assertNotIn("boom <script>", doc)
        self.assertNotIn("<script>", doc)
        self.assertIn("**Status:** warning\n", doc)

    def test_unrecognized_status_token_fails_loudly_as_render_error(self) -> None:
        # comparator contract drift: an unknown --status-out token must not
        # silently classify the pair as healthy — it renders a render-error
        # note and says so on stderr, and the table is held back
        self.default_tree()
        self.write_report("fraud-loadtest.json", report(p99=1_000_000))
        stub = self.write_stub(
            "shift  # the compare-loadtest.py path\n"
            "while [[ $# -gt 0 ]]; do\n"
            '  if [[ "$1" == "--status-out" ]]; then\n'
            '    printf "bogus-token\\n" >"$2"\n'
            "    shift 2\n"
            "  else\n"
            "    shift\n"
            "  fi\n"
            "done\n"
            'echo "### stub table"\n'
        )
        result, doc = self.render(extra_env={"PYTHON": str(stub)})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("fraud-loadtest: render-error", doc)
        self.assertIn("unrecognized status token `bogus-token`", doc)
        self.assertIn(
            "compare-baselines: unexpected status token 'bogus-token' "
            "for fraud-loadtest.json",
            result.stderr,
        )
        self.assertNotIn("stub table", doc)
        self.assertIn("**Status:** warning\n", doc)

    def test_paths_under_repo_root_render_relative(self) -> None:
        repo_target = ROOT / "target"
        repo_target.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=str(repo_target)) as tmp:
            # Point the fixture tree at the temp dir inside the repo, so
            # the base class's one runner is what invokes the aggregator.
            self.baselines = Path(tmp) / "baselines"
            self.reports = Path(tmp) / "reports"
            self.baselines.mkdir()
            self.reports.mkdir()
            self.write_baseline("fraud-loadtest.json", report(p99=1_000_000))
            self.write_report("fraud-loadtest.json", report(p99=1_100_000))
            result = self.run_script("--render-only")
            self.assertEqual(result.returncode, 0, result.stderr)
            doc = (self.reports / "loadtest-comparison.md").read_text()
            rel = f"target/{Path(tmp).name}"
            self.assertIn(
                f"`{rel}/baselines/fraud-loadtest.json` → "
                f"`{rel}/reports/fraud-loadtest.json`",
                doc,
            )
            self.assertNotIn(f"`{ROOT}", doc)


class CompareBaselinesGateTests(CompareBaselinesTestBase):
    """No --render-only flag: gate membership is the registry's gated
    records — fraud and identity unconditionally."""

    def test_gate_fails_on_missing_reports(self) -> None:
        self.default_tree()
        result = self.run_script()
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "compare-baselines: missing current report fraud-loadtest.json", result.stderr
        )
        self.assertIn(
            "compare-baselines: missing current report identity-loadtest.json",
            result.stderr,
        )
        # checkpoint and memory-shape left the registry: the gate never
        # mentions them, even with a checkpoint baseline committed
        self.assertNotIn("fraud-checkpoint", result.stderr + result.stdout)
        self.assertNotIn("memory-shape", result.stderr + result.stdout)
        # gate mode writes no combined document
        self.assertFalse((self.reports / "loadtest-comparison.md").exists())

    def test_gate_passes_with_fraud_and_identity_reports(self) -> None:
        self.default_tree()
        self.write_report(
            "fraud-loadtest.json",
            report(p99=1_100_000, ops_per_sec=95.0, logical_used_bytes=1050),
        )
        self.write_report(
            "identity-loadtest.json", report(p99=1_000_000, kind="identity")
        )
        # a stray checkpoint report (logical bytes blown out) must not
        # re-enter the gate: membership is the registry's, never the tree's
        self.write_report(
            "fraud-checkpoint.json", checkpoint_report(logical=99_999_999)
        )
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.count("compare-loadtest: ok"), 2)
        self.assertNotIn("fraud-checkpoint", result.stderr + result.stdout)
        self.assertNotIn("memory-shape", result.stderr + result.stdout)

    def test_gate_propagates_comparator_failures(self) -> None:
        self.default_tree()
        self.write_report(
            "fraud-loadtest.json", report(p99=1_000_000, ops_per_sec=50.0)
        )
        self.write_report(
            "identity-loadtest.json", report(p99=1_000_000, kind="identity")
        )
        result = self.run_script()
        self.assertEqual(result.returncode, 1)
        self.assertIn("ops_per_sec", result.stderr)

    def test_gate_ignores_memory_shape_even_when_its_baseline_is_frozen(self) -> None:
        # memory-shape left the registry (re-entry is a future decision):
        # a frozen baseline plus a report whose RSS ratio would breach must
        # not re-enter the gate
        self.default_tree()
        self.write_report(
            "fraud-loadtest.json",
            report(p99=1_100_000, ops_per_sec=95.0, logical_used_bytes=1050),
        )
        self.write_report(
            "identity-loadtest.json", report(p99=1_000_000, kind="identity")
        )
        self.write_baseline(
            "memory-shape-loadtest.json", memory_report(logical=1000, ratio=2.0)
        )
        self.write_report(
            "memory-shape-loadtest.json", memory_report(logical=1000, ratio=9.9)
        )
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.count("compare-loadtest: ok"), 2)
        self.assertNotIn("memory-shape", result.stderr + result.stdout)

    def test_unknown_arguments_are_rejected(self) -> None:
        result = self.run_script("--render-noly")
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage: compare-baselines.sh [--render-only]", result.stderr)
        result = self.run_script("--render-only", "extra")
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage: compare-baselines.sh [--render-only]", result.stderr)


if __name__ == "__main__":
    unittest.main()
