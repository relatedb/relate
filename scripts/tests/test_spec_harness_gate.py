"""Unit-test the gate and baseline-writer CLI
(`scripts/spec-harness-gate.py`, ADR-0661): the two mode flows' exit
codes (0 green / 1 red / 2 no-verdict, fail-closed), the writer mode's
ordering (a corrupt previous baseline resolves before anything is
written) and its delta line, the IP boundary — a red verdict publishes
counts and categories only, never a scenario id or corpus path — and
the argparse mode contracts.

The document and comparison contracts are tested in
`test_spec_harness_ids.py` and `test_spec_harness_baseline.py`; the
shared fixtures live in `spec_harness_fixtures.py`.
"""

import json
import tempfile
import unittest
from pathlib import Path

from spec_harness_fixtures import (
    ENGINE_SHA,
    HARNESS_SHA,
    OTHER_SHA,
    baseline,
    baseline_script,
    collect,
    errored,
    failure,
    features,
    gate_script,
    ids_of,
    nondet,
    run_script,
    skip,
    summary,
)


class Writer(unittest.TestCase):
    def emit(self, data, engine=ENGINE_SHA, harness=HARNESS_SHA,
             previous_doc=None, previous_path=None):
        """Run the writer mode on a fixture in a temp dir; returns the
        captured result and the written baseline document (read before
        the temp dir goes away), or None when the writer wrote nothing.
        `previous_doc` is a baseline document written for the run;
        `previous_path` is a path used as-is — the two spellings of the
        --previous-baseline flag this suite needs (a superseded baseline,
        and a file that is deliberately absent or corrupt), as two
        parameters, never one shape-sniffed value. The two spellings are
        mutually exclusive: a run that names both is a fixture bug."""
        if previous_doc is not None and previous_path is not None:
            raise ValueError(
                "one previous-baseline spelling per run: doc or path"
            )
        with tempfile.TemporaryDirectory() as tmp:
            summary_path = Path(tmp) / "summary.json"
            summary_path.write_text(json.dumps(data))
            out_path = Path(tmp) / "engine-main.json"
            args = [
                str(summary_path),
                "--emit-baseline", str(out_path),
                "--engine-sha", engine, "--harness-sha", harness,
            ]
            if previous_doc is not None:
                prev_path = Path(tmp) / "previous.json"
                prev_path.write_text(json.dumps(previous_doc))
                args += ["--previous-baseline", str(prev_path)]
            elif previous_path is not None:
                args += ["--previous-baseline", str(previous_path)]
            result = run_script(args)
            # Existence alone decides: a failed writer must leave no
            # file, and each case asserts its own exit code.
            written = (
                json.loads(out_path.read_text())
                if out_path.exists()
                else None
            )
            return result, written

    def test_the_writer_round_trips_into_a_baseline_the_gate_reads(self):
        data = summary(
            features(
                failures=[failure("One")],
                nondeterministic=[nondet("Wobbly")],
                skips=[skip("Admin")],
                errored=[errored("Crashed")],
            ),
            totals={"passed": 50, "failed": 1, "error": 1},
        )
        result, written = self.emit(data)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(written["schema"], baseline_script.BASELINE_SCHEMA)
        self.assertEqual(written["engine_sha"], ENGINE_SHA)
        self.assertEqual(written["harness_sha"], HARNESS_SHA)
        self.assertEqual(written["failing"], ids_of(["One"]))
        self.assertEqual(written["errored"], ids_of(["Crashed"]))
        self.assertEqual(written["nondeterministic"], ids_of(["Wobbly"]))
        self.assertEqual(written["skipped"], ids_of(["Admin"]))
        self.assertEqual(written["totals"]["passed"], 50)
        # The writer's line is counts only (ADR-0661 §4).
        self.assertIn("1 failing, 1 errored", result.stdout)
        # Round trip: the gate reads its own output green against the
        # same run — writer and gate cannot drift (one spelling, twice).
        parsed, diags = gate_script.Baseline.from_json(written)
        self.assertEqual(diags, [])
        ids = collect(data)
        verdict = gate_script.gate_verdict(ids, parsed)
        self.assertFalse(verdict.red)

    def test_an_older_harness_summary_emits_an_empty_skip_rollup(self):
        data = summary(features(failures=[failure("One")]))
        del data["skip_categories"]
        result, written = self.emit(data)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(written["skip_categories"], {})

    def test_the_watchdog_line_reports_a_regression_with_categories(self):
        previous = baseline(failing=ids_of(["One"]))
        data = summary(
            features(failures=[failure("Two", "type-error")],
                     errored=[errored("Crashed")])
        )
        result, _ = self.emit(data, previous_doc=previous)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("delta new=2 fixed=1 harness_moved=0", result.stdout)
        self.assertIn("new_categories=no-category:1,type-error:1", result.stdout)

    def test_the_watchdog_line_marks_harness_movement(self):
        # The discriminator ADR-0661 §6 records: same engine, moved
        # corpus — the triage fact for a main-run delta.
        previous = baseline(failing=ids_of(["One"]), harness=OTHER_SHA)
        data = summary(features(failures=[failure("One")]))
        result, _ = self.emit(data, previous_doc=previous)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("delta new=0 fixed=0 harness_moved=1", result.stdout)

    def test_a_first_write_reports_no_previous_baseline(self):
        # A missing previous baseline is the bootstrap, green by design;
        # the previous path is deliberately not written.
        data = summary(features(failures=[failure("One")]))
        with tempfile.TemporaryDirectory() as tmp:
            missing = Path(tmp) / "does-not-exist.json"
            result, _ = self.emit(data, previous_path=missing)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no previous baseline — first write", result.stdout)

    def test_a_corrupt_previous_baseline_is_not_the_first_write(self):
        # Absence and corruption are different facts (the no-silent-
        # fallbacks rule): a previous baseline that is present but not
        # JSON is exit 2, never a green first-write line that hides the
        # corruption from the watchdog.
        data = summary(features(failures=[failure("One")]))
        with tempfile.TemporaryDirectory() as tmp:
            corrupt = Path(tmp) / "previous.json"
            corrupt.write_text("{not json")
            result, written = self.emit(data, previous_path=corrupt)
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("not valid JSON", result.stderr)
        self.assertNotIn("first write", result.stdout)
        # The corruption resolves BEFORE the emit: a failed run leaves
        # no new baseline and no 'wrote' line behind.
        self.assertIsNone(written)
        self.assertNotIn("wrote", result.stdout)

    def test_a_non_utf8_document_is_exit_two_not_a_traceback(self):
        # The decode happens at the read boundary: a present-but-non-
        # UTF-8 document must land in the shape arm (exit 2, named), not
        # escape as a raw UnicodeDecodeError the step would read as a
        # red verdict — the fail-closed taxonomy (ADR-0661 §5).
        data = summary(features(failures=[failure("One")]))
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / "previous.json"
            binary.write_bytes(b"\xff\xfe{not utf-8 at all")
            result, _ = self.emit(data, previous_path=binary)
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("not valid JSON", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_a_bad_engine_sha_is_a_usage_error(self):
        data = summary(features())
        result, _ = self.emit(data, engine="abc123")
        self.assertEqual(result.returncode, 2)
        self.assertIn("not a 40-hex sha", result.stderr)


class ExitCodes(unittest.TestCase):
    def run_gate(self, summary_data, baseline_data, extra_args=(), env=None):
        """Run the CLI on a fixture pair in a temp dir, with the CI
        reporting env scrubbed and per-case additions applied on top.
        `baseline_data=None` leaves the baseline path unwritten — the
        fail-closed missing-baseline case."""
        with tempfile.TemporaryDirectory() as tmp:
            summary_path = Path(tmp) / "summary.json"
            summary_path.write_text(json.dumps(summary_data))
            baseline_path = Path(tmp) / "engine-main.json"
            if baseline_data is not None:
                baseline_path.write_text(json.dumps(baseline_data))
            return run_script(
                [
                    str(summary_path),
                    "--baseline", str(baseline_path),
                    "--harness-sha", HARNESS_SHA,
                    *extra_args,
                ],
                env=env,
            )

    def test_a_green_run_exits_zero_with_the_table_and_status(self):
        result = self.run_gate(
            summary(features(failures=[failure("One")])),
            baseline(failing=ids_of(["One"])),
        )
        self.assertEqual(result.returncode, 0)
        self.assertIn("## Spec-harness gate — 🟢", result.stdout)
        self.assertIn("spec-harness-gate:", result.stderr)

    def test_a_red_run_exits_one_with_the_counts_only(self):
        result = self.run_gate(
            summary(features(failures=[failure("Two", "type-error")])),
            baseline(failing=ids_of(["One"])),
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "**1 new failing scenarios vs main** (1 assertion failures, "
            "0 step crashes)",
            result.stdout,
        )
        # The IP boundary (ADR-0661 §4): counts and categories render;
        # scenario ids and corpus paths never do — not on stdout, not on
        # stderr, not in the table.
        for stream in (result.stdout, result.stderr):
            self.assertNotIn("Two", stream)
            self.assertNotIn("a.feature", stream)
        self.assertIn("type-error ×1", result.stdout)

    def test_a_broken_summary_exits_two_without_a_verdict(self):
        result = self.run_gate(
            summary(features()) | {"totals": {}}, baseline()
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("spec-harness-gate:", result.stderr)

    def test_a_missing_baseline_exits_two_with_the_bootstrap_remedy(self):
        result = self.run_gate(summary(features()), baseline_data=None)
        self.assertEqual(result.returncode, 2)
        self.assertIn("bootstrap per ADR-0661", result.stderr)

    def test_a_revision_mismatch_under_the_pin_assertion_exits_two(self):
        # The pinned-checkout assertion (ADR-0661 §3): the checkout must
        # be the revision the baseline records.
        result = self.run_gate(
            summary(features()),
            baseline(harness=OTHER_SHA),
            extra_args=["--require-baseline-revision"],
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("like-for-like pin", result.stderr)

    def test_the_matching_revision_passes_the_pin_assertion(self):
        result = self.run_gate(
            summary(features()),
            baseline(harness=HARNESS_SHA),
            extra_args=["--require-baseline-revision"],
        )
        self.assertEqual(result.returncode, 0)

    def test_mode_flags_are_mutually_exclusive(self):
        with tempfile.TemporaryDirectory() as tmp:
            summary_path = Path(tmp) / "summary.json"
            summary_path.write_text(json.dumps(summary(features())))
            out_path = Path(tmp) / "engine-main.json"
            # Both modes named, then neither: the one required-choice
            # group owns the exactly-one-of invariant — each case takes
            # argparse's exit 2 with its own diagnostic, never a silent
            # default mode. The third case is a mode-exclusive flag
            # under the wrong mode: accepted-and-ignored is the shape
            # these checks exist to kill.
            cases = [
                ([str(summary_path), "--baseline", str(out_path),
                  "--emit-baseline", str(out_path)],
                 "not allowed with"),
                ([str(summary_path)], "required"),
                ([str(summary_path), "--baseline", str(out_path),
                  "--harness-sha", HARNESS_SHA,
                  "--engine-sha", ENGINE_SHA],
                 "belong to --emit-baseline"),
            ]
            for argv, needle in cases:
                result = run_script(argv)
                self.assertEqual(result.returncode, 2, argv)
                self.assertIn(needle, result.stderr)

    def test_the_step_summary_append_lands_at_the_targeted_path(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "step-summary.md"
            self.run_gate(
                summary(features(failures=[failure("One")])),
                baseline(failing=ids_of(["One"])),
                env={"GITHUB_STEP_SUMMARY": str(target)},
            )
            written = target.read_text()
            self.assertIn("## Spec-harness gate — 🟢", written)
            self.assertIn("1 carried as not passing", written)


if __name__ == "__main__":
    unittest.main()
