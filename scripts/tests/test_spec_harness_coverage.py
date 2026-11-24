"""Unit-test the ADR-0482 spec-harness coverage report
(scripts/spec-harness-coverage.py): the shape contract (the @fails
exclusion invariant, one totals vocabulary, the two category rollups), the
coverage math, the traffic-light bands, the markdown rendering (including
the skip breakdown), the step-summary append, and the exit codes — 0 for
any coverage number (reporting, not gating), 1 only for a summary the
contract does not recognize.

The subprocess helper scrubs the Actions reporting pair the way the
sibling suites do (scripts/tests/child_env.py, the one home of the
policy): the script under test appends to `$GITHUB_STEP_SUMMARY` whenever
it is set, and this suite runs inside a CI step where the runner sets it —
an unscrubbed env would write fabricated coverage rows into the job's real
step summary.
"""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from child_env import REPORTING_ENV, child_env
from script_loader import ROOT, load

coverage_script = load(
    "spec_harness_coverage", ROOT / "scripts" / "spec-harness-coverage.py"
)

SCRIPT = ROOT / "scripts" / "spec-harness-coverage.py"

# The real patched-harness summary against engine main 49985dbe
# (2026-11-02): categories sum to exactly the genuine-failure count.
REAL_TOTALS = {
    "passed": 13097,
    "failed": 960,
    "neo4j_expected_fail": 178,
    "nondeterministic": 7,
    "skipped": 1766,
}
REAL_CATEGORIES = {
    "missing-validation": 425,
    "result-temporal-fidelity": 314,
    "type-error": 66,
    "result-mismatch": 60,
    "notification-missing": 56,
    "syntax-error": 15,
    "parameter-binding": 11,
    "setup-failed-type": 4,
    "result-vector": 4,
    "semantic-error": 2,
    "setup-failed-syntax": 2,
    "setup-failed-semantic": 1,
}
# The per-reason rollup behind REAL_TOTALS's `skipped: 1766` (the harness's
# `skip_categories`): the by-design graphengine suite, the Cypher-5-era
# expectations, and the skip-list's process-aborting skips.
REAL_SKIPS = {
    "graphengine": 1024,
    "cypher-5": 612,
    "skip-list": 130,
}


def summary(totals=None, categories=None, skips=None, suites=None):
    """A summary.json in the harness's shape. The totals vocabulary is the
    module's own [`TOTALS_KEYS`] (every bucket present, zero unless
    overridden), so a new bucket is exercised by default and the fixture
    cannot drift from the vocabulary. Categories default to the
    genuine-failure breakdown that sums to `failed`; `skips` defaults to
    the empty rollup an older harness emits; `suites` defaults to empty
    (no per-suite buckets)."""
    values = {key: 0 for key, _ in coverage_script.TOTALS_KEYS}
    values.update(totals or {})
    if categories is None:
        failed = values["failed"]
        categories = {} if failed == 0 else {"result-mismatch": failed}
    return {
        "generated": "2026-11-02T00:00:00+00:00",
        "command": "behave",
        "totals": values,
        "suites": {} if suites is None else suites,
        "failure_categories": categories,
        "skip_categories": {} if skips is None else skips,
        "features": {},
    }


def summary_absent(key, totals=None):
    """A summary whose totals bucket `key` is absent — the one
    absence-fixture policy: build the prefilled fixture, then delete the
    key the way a real summary omits it (delete, don't zero)."""
    fixture = summary(totals)
    del fixture["totals"][key]
    return fixture


def parse(data):
    """The parsed Summary for a contract-honoring fixture (the shape
    tests call `parse_summary` directly for the broken shapes)."""
    parsed, diags = coverage_script.parse_summary(data)
    assert parsed is not None, diags
    return parsed


class Shape(unittest.TestCase):
    def assert_shape_failure(self, data, needle):
        """The one shape-failure assertion every broken-shape case shares:
        the parse yields no Summary and a diagnostic naming `needle`."""
        parsed, diags = coverage_script.parse_summary(data)
        self.assertIsNone(parsed)
        self.assertTrue(any(needle in d for d in diags), diags)

    def test_a_contract_honoring_summary_parses(self):
        parsed, diags = coverage_script.parse_summary(
            summary(REAL_TOTALS, REAL_CATEGORIES, REAL_SKIPS)
        )
        self.assertEqual(diags, [])
        self.assertEqual((parsed.passed, parsed.failed), (13097, 960))
        self.assertEqual(parsed.size, 14057)
        self.assertEqual(parsed.context.get("neo4j_expected_fail"), 178)
        # The non-zero filter is the parser's policy: a zero context
        # bucket is not reported at all.
        self.assertNotIn("untested", parsed.context)
        # The skip breakdown rides the summary beside the skipped total —
        # it is what makes that one number legible.
        self.assertEqual(parsed.skip_categories, REAL_SKIPS)

    def test_categories_summing_past_failed_breaks_the_fails_exclusion(self):
        # The pre-patch harness shape: @fails failures inside the rollup.
        self.assert_shape_failure(
            summary({"failed": 960}, {"result-mismatch": 1138}), "ADR-0482"
        )

    def test_a_missing_required_total_is_a_shape_failure_not_a_zero(self):
        self.assert_shape_failure(
            summary_absent("failed", REAL_TOTALS), "totals.failed"
        )

    def test_a_misshapen_context_total_is_a_shape_failure(self):
        # Context buckets are never gated on, but present-but-malformed
        # still fails loudly — a shape change must not render into the
        # step summary unvalidated.
        self.assert_shape_failure(
            summary({**REAL_TOTALS, "skipped": "many"}, REAL_CATEGORIES),
            "totals.skipped",
        )

    def test_a_null_context_total_is_a_shape_failure_not_an_absence(self):
        # A JSON null is *present*: the absent bucket and the malformed one
        # are different facts, and only absence is allowed.
        self.assert_shape_failure(
            summary({**REAL_TOTALS, "skipped": None}, REAL_CATEGORIES),
            "totals.skipped",
        )

    def test_an_absent_context_total_is_fine(self):
        parsed, diags = coverage_script.parse_summary(
            summary_absent("untested", {"passed": 100, "failed": 0})
        )
        self.assertEqual(diags, [])
        self.assertNotIn("untested", parsed.context)
        self.assertEqual(parsed.context, {})

    def test_a_non_identifier_category_name_is_rejected(self):
        # The names travel into the bot's PR comment — anything but the
        # harness's kebab-case shape is a shape failure.
        self.assert_shape_failure(
            summary({"passed": 1, "failed": 1}, {"bad name!": 1}),
            "failure_categories",
        )

    def test_a_missing_failure_categories_is_named_as_absence(self):
        # Absence is named as absence, like the totals walk: a missing
        # member is a different fact from a present-but-malformed one.
        fixture = summary({"passed": 10, "failed": 2})
        del fixture["failure_categories"]
        self.assert_shape_failure(fixture, "failure_categories is missing")

    def test_a_null_failure_categories_is_malformed_not_missing(self):
        # A JSON null is *present*, so it is malformed, not absent — the
        # same distinction the totals walk draws for a null context bucket.
        fixture = summary({"passed": 10, "failed": 2})
        fixture["failure_categories"] = None
        self.assert_shape_failure(fixture, "is not a map")

    def test_a_non_identifier_skip_category_name_is_rejected(self):
        # Same shape contract as the failure rollup: the names travel into
        # the bot's PR comment.
        self.assert_shape_failure(
            summary({"passed": 1, "failed": 1}, skips={"bad name!": 1}),
            "skip_categories",
        )

    def test_a_misshapen_skip_category_count_is_rejected(self):
        self.assert_shape_failure(
            summary({"passed": 1, "failed": 1}, skips={"graphengine": "many"}),
            "skip_categories",
        )

    def test_an_absent_skip_categories_rollup_is_fine(self):
        # Older harnesses predate the member: absence is not a shape
        # failure, and the summary still parses with an empty breakdown.
        fixture = summary({"passed": 10, "failed": 0}, skips=REAL_SKIPS)
        del fixture["skip_categories"]
        parsed, diags = coverage_script.parse_summary(fixture)
        self.assertEqual(diags, [])
        self.assertEqual(parsed.skip_categories, {})

    def test_a_null_skip_categories_is_malformed_not_absent(self):
        # A JSON null is *present*, so it is malformed — the same
        # distinction the totals walk draws for a null context bucket.
        fixture = summary({"passed": 10, "failed": 0})
        fixture["skip_categories"] = None
        self.assert_shape_failure(fixture, "skip_categories")

    def test_a_malformed_error_total_is_a_shape_failure(self):
        # The errored bucket is counted, so a malformed value is a shape
        # failure like any required total — not a silent zero.
        self.assert_shape_failure(
            summary({**REAL_TOTALS, "error": "many"}, REAL_CATEGORIES),
            "totals.error",
        )

    def test_an_unknown_total_is_a_shape_failure(self):
        # The vocabulary boundary is exhaustive: a harness-side rename
        # (e.g. skipped -> not_run) must not be silently ignored.
        self.assert_shape_failure(
            summary({**REAL_TOTALS, "not_run": 5}, REAL_CATEGORIES),
            "totals.not_run",
        )

    def test_a_non_object_summary_is_rejected_authored(self):
        self.assert_shape_failure([1, 2, 3], "not an object")

    def test_an_empty_run_is_a_shape_failure(self):
        self.assert_shape_failure(summary({"passed": 0, "failed": 0}), "no scenarios")

    def test_no_totals_object_fails_loudly(self):
        self.assert_shape_failure({"totals": None}, "totals object")


class Coverage(unittest.TestCase):
    def test_coverage_is_passed_over_executed(self):
        self.assertAlmostEqual(
            parse(summary(REAL_TOTALS, REAL_CATEGORIES)).coverage,
            13097 / 14057 * 100,
        )
        self.assertEqual(parse(summary({"passed": 10})).coverage, 100.0)

    def test_errored_scenarios_count_against_coverage(self):
        # A step crash is a genuine failure: in the denominator, in the
        # failure share the bands read, and in the headline count.
        parsed = parse(summary({"passed": 90, "failed": 5, "error": 6}))
        self.assertEqual(parsed.size, 101)
        self.assertEqual(parsed.defect_count, 11)
        self.assertAlmostEqual(parsed.failure_rate, 11 / 101 * 100)
        self.assertEqual(parsed.light, "🔴")
        self.assertIn("11 genuine failures", parsed.headline)
        # Behave reports the bucket only when a step raised.
        self.assertEqual(
            parse(summary_absent("error", {"passed": 10})).errored, 0
        )

    def test_the_light_bands_are_green_yellow_and_red(self):
        # The bands (ADR-0482 §4): green at zero, yellow through an exact
        # 10% failure share, red beyond it.
        self.assertEqual(parse(summary({"passed": 14057})).light, "🟢")
        self.assertEqual(parse(summary({"passed": 90, "failed": 10})).light, "🟡")
        self.assertEqual(
            parse(summary(REAL_TOTALS, REAL_CATEGORIES)).light, "🟡"
        )
        self.assertEqual(parse(summary({"passed": 89, "failed": 11})).light, "🔴")


class Rendering(unittest.TestCase):
    def test_the_legend_is_rendered_from_the_band_table(self):
        # The threshold is stated once (YELLOW_LIMIT_PERCENT / BANDS): the
        # legend interpolates it rather than re-spelling "10%".
        text = coverage_script.render_markdown(
            parse(summary({"passed": 10, "failed": 2})), harness_sha=None
        )
        self.assertIn(
            f"bands: 🟢 zero failures · 🟡 failure share ≤ "
            f"{coverage_script.YELLOW_LIMIT_PERCENT:g}% · 🔴 beyond",
            text,
        )

    def test_markdown_carries_the_traffic_light_context_and_categories(self):
        # 2 of 12 executed = 16.67% failure share — the red band.
        text = coverage_script.render_markdown(
            parse(summary(
                {"passed": 10, "failed": 2, "neo4j_expected_fail": 3,
                 "skipped": 5},
                {"result-mismatch": 2},
            )),
            harness_sha="25982e1",
            run_seconds=192,
        )
        for needle in ("## Spec-harness coverage — 🔴", "83.33%",
                       "scenarios run: **12**",
                       "suite runtime: 3m 12s",
                       "neo4j_expected_fail (not counted): 3",
                       "skipped (not counted): 5",
                       "harness ref: `25982e1`", "| result-mismatch | 2 |"):
            self.assertIn(needle, text, text)
        # Zero-count context buckets are the parser's policy to omit, so
        # they never reach the table.
        self.assertNotIn("nondeterministic", text)

    def test_the_skip_breakdown_renders_as_its_own_table(self):
        # The `skipped` bullet stays one total; the breakdown behind it is
        # its own table, ordered by descending count then name (the
        # failure table's order).
        text = coverage_script.render_markdown(
            parse(summary({"passed": 10, "skipped": 1766}, skips=REAL_SKIPS)),
            harness_sha=None,
        )
        self.assertIn("skipped (not counted): 1766", text)
        self.assertIn("| skipped category | count |", text)
        self.assertIn("| graphengine | 1024 |", text)
        self.assertIn("| cypher-5 | 612 |", text)
        self.assertLess(text.index("| graphengine |"), text.index("| cypher-5 |"))
        # The failure table keeps its own header: the two breakdowns never
        # share one table.
        self.assertNotIn("| failure category | count |", text)

    def test_the_skip_table_renders_only_when_the_rollup_has_rows(self):
        # An empty (or absent) rollup renders no table — an empty table
        # would be noise, not visibility.
        empty = coverage_script.render_markdown(
            parse(summary({"passed": 10, "skipped": 5})), harness_sha=None
        )
        self.assertIn("skipped (not counted): 5", empty)
        self.assertNotIn("skipped category", empty)

    def test_the_errored_row_renders_only_when_nonzero(self):
        # Value gating is the policy: the row appears for a non-zero
        # count, whether the bucket was present-non-zero or absent.
        text = coverage_script.render_markdown(
            parse(summary({"passed": 10, "failed": 2, "error": 3})),
            harness_sha=None,
        )
        self.assertIn("(passed 10, failed 2, errored 3)", text)
        # The breakdown reconciles with the headline: 2 categorized + 3
        # uncategorized = the 5 genuine failures the headline counts.
        self.assertIn("| errored (no category) | 3 |", text)
        clean = coverage_script.render_markdown(
            parse(summary({"passed": 10, "failed": 2})), harness_sha=None
        )
        self.assertNotIn("errored", clean)

    def test_the_preview_corpus_line_renders_from_its_suite_bucket(self):
        # The preview-gated suite is outside the totals by contract; its
        # per-suite bucket renders as its own not-counted line.
        text = coverage_script.render_markdown(
            parse(summary(
                {"passed": 10, "failed": 2},
                suites={"cip-preview": {"passed": 90, "failed": 5}},
            )),
            harness_sha=None,
            preview={"passed": 90, "failed": 5},
        )
        self.assertIn(
            "preview corpus (not counted): **95** run (passed 90, failed 5)",
            text,
        )

    def test_the_preview_line_counts_errored_scenarios(self):
        text = coverage_script.render_markdown(
            parse(summary({"passed": 10})),
            harness_sha=None,
            preview={"passed": 8, "failed": 1, "error": 1},
        )
        self.assertIn(
            "preview corpus (not counted): **10** run "
            "(passed 8, failed 1, errored 1)",
            text,
        )

    def test_no_preview_bucket_renders_no_preview_line(self):
        text = coverage_script.render_markdown(
            parse(summary({"passed": 10})), harness_sha=None
        )
        self.assertNotIn("preview corpus", text)

    def test_preview_counts_reads_only_a_well_shaped_bucket(self):
        read = coverage_script.preview_counts
        self.assertIsNone(read({}))
        self.assertIsNone(read({"suites": {}}))
        self.assertIsNone(read({"suites": {"cip-preview": {}}}))
        self.assertIsNone(read({"suites": {"cip-preview": "nope"}}))
        self.assertIsNone(
            read({"suites": {"cip-preview": {"passed": "90"}}})
        )
        self.assertIsNone(
            read({"suites": {"cip-preview": {"passed": True}}})
        )
        self.assertEqual(
            read({"suites": {"cip-preview": {"passed": 90, "failed": 5}}}),
            {"passed": 90, "failed": 5},
        )

    def test_the_runtime_row_appears_only_when_measured(self):
        parsed = parse(summary({"passed": 10, "failed": 2}))
        self.assertIn("suite runtime", coverage_script.render_markdown(
            parsed, harness_sha=None, run_seconds=47
        ))
        self.assertNotIn("suite runtime:", coverage_script.render_markdown(
            parsed, harness_sha=None
        ))

    def test_format_seconds_has_the_one_spelling(self):
        self.assertEqual(coverage_script.format_seconds(47), "47s")
        self.assertEqual(coverage_script.format_seconds(192), "3m 12s")
        self.assertEqual(coverage_script.format_seconds(3720), "1h 2m")
        self.assertEqual(coverage_script.format_seconds(0), "0s")


class ExitCodes(unittest.TestCase):
    def run_script(self, data, env=None, extra_args=()):
        """Run the script under test on a fixture summary in a temp dir,
        with the CI reporting env scrubbed (see the module docstring) and
        any per-case additions applied on top."""
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "summary.json"
            path.write_text(json.dumps(data))
            return subprocess.run(
                [sys.executable, str(SCRIPT), str(path), *extra_args],
                capture_output=True, text=True,
                env=child_env(REPORTING_ENV, env),
            )

    def test_below_100_reports_the_light_without_failing(self):
        # Reporting, not gating (ADR-0482 §4): a low number never fails a
        # PR — the light carries the signal. 960/14057 = 6.83% → yellow.
        result = self.run_script(summary(REAL_TOTALS, REAL_CATEGORIES))
        self.assertEqual(result.returncode, 0)
        self.assertIn("🟡", result.stdout)
        self.assertIn("960 genuine failures (6.83%", result.stderr)

    def test_100_exits_zero_with_the_green_light(self):
        result = self.run_script(summary({"passed": 14057}))
        self.assertEqual(result.returncode, 0)
        self.assertIn("🟢", result.stdout)
        self.assertIn("100.00%", result.stdout)
        # The zero-failure arm's own tail, so deleting that branch fails
        # the suite (the shared lead would survive).
        self.assertIn("— complete", result.stdout)

    def test_the_step_summary_append_lands_at_the_targeted_path(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "step-summary.md"
            result = self.run_script(
                summary({"passed": 10, "failed": 2}),
                env={"GITHUB_STEP_SUMMARY": str(target)},
            )
            self.assertEqual(result.returncode, 0)
            written = target.read_text()
            self.assertIn("## Spec-harness coverage — 🔴", written)
            self.assertIn("83.33%", written)

    def test_the_step_summary_append_accumulates_across_runs(self):
        # The write is an append, not a truncate: a lane that runs the
        # report twice keeps both tables.
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "step-summary.md"
            env = {"GITHUB_STEP_SUMMARY": str(target)}
            self.run_script(summary({"passed": 10, "failed": 2}), env=env)
            self.run_script(summary({"passed": 12, "failed": 0}), env=env)
            written = target.read_text()
            self.assertEqual(written.count("## Spec-harness coverage"), 2)

    def test_the_comment_payload_excludes_the_status_line(self):
        # The lane pipes stdout into the PR comment: the status line must
        # travel on stderr so the comment carries the table only.
        result = self.run_script(summary(REAL_TOTALS, REAL_CATEGORIES))
        self.assertNotIn("spec-harness-coverage:", result.stdout)
        self.assertIn("spec-harness-coverage:", result.stderr)

    def test_a_broken_shape_exits_one_without_a_verdict(self):
        result = self.run_script({"totals": {}})
        self.assertEqual(result.returncode, 1)
        self.assertIn("spec-harness-coverage:", result.stderr)

    def test_the_runtime_flag_reaches_the_report(self):
        result = self.run_script(
            summary({"passed": 10, "failed": 2}),
            extra_args=("--run-seconds", "252"),
        )
        self.assertEqual(result.returncode, 0)
        self.assertIn("suite runtime: 4m 12s", result.stdout)

    def test_a_negative_runtime_is_rejected_at_the_parse_boundary(self):
        # A usage error keeps argparse's exit 2 (the one path every bad
        # flag takes), never the report's own exit 1.
        result = self.run_script(
            summary({"passed": 10, "failed": 2}),
            extra_args=("--run-seconds", "-5"),
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("non-negative", result.stderr)


if __name__ == "__main__":
    unittest.main()
