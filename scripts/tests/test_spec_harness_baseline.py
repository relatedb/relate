"""Unit-test the baseline document and the comparison
(`scripts/spec_harness_baseline.py`, ADR-0661 §1/§2): the writer's
invariants on every read (schema pin, shas, sorted-unique id lists,
the totals vocabulary — corruption is a hard failure, never a
default), and the set semantics of the verdict (a fix-N-break-N swap
is red; excusals for skip/nondet/@fails; the partition by failure
kind).

The shared fixtures live in `spec_harness_fixtures.py`.
"""

import unittest

from spec_harness_fixtures import (
    ENGINE_SHA,
    HARNESS_SHA,
    assert_parse_failure,
    baseline,
    baseline_script,
    collect,
    errored,
    features,
    failure,
    gate_script,
    ids_of,
    parse_baseline,
    summary,
)


class BaselineShape(unittest.TestCase):
    def assert_shape_failure(self, data, needle):
        assert_parse_failure(baseline_script.Baseline.from_json, data, needle)

    def test_a_contract_honoring_baseline_parses(self):
        parsed = parse_baseline(baseline(failing=ids_of(["One"])))
        self.assertEqual(parsed.engine_sha, ENGINE_SHA)
        self.assertEqual(
            parsed.failing, {"features/tck/a.feature::One"}
        )
        self.assertEqual(parsed.not_passing, parsed.failing)

    def test_a_non_object_baseline_is_rejected(self):
        self.assert_shape_failure([1], "not an object")

    def test_a_foreign_schema_is_refused_loudly(self):
        self.assert_shape_failure(
            baseline(schema=2), "schema 2 is not 1"
        )

    def test_a_short_sha_is_a_shape_failure(self):
        self.assert_shape_failure(
            baseline(engine="abc123"), "40-hex sha"
        )

    def test_a_missing_sha_is_a_shape_failure(self):
        fixture = baseline()
        del fixture["harness_sha"]
        self.assert_shape_failure(fixture, "harness_sha")

    def test_a_missing_totals_object_is_a_shape_failure(self):
        fixture = baseline()
        fixture["totals"] = None
        self.assert_shape_failure(fixture, "totals object")

    def test_a_totals_key_outside_the_vocabulary_is_a_shape_failure(self):
        fixture = baseline()
        fixture["totals"]["not_run"] = 3
        self.assert_shape_failure(fixture, "totals.not_run")

    def test_a_missing_category_rollup_is_a_shape_failure(self):
        fixture = baseline()
        del fixture["failure_categories"]
        self.assert_shape_failure(fixture, "failure_categories")

    def test_a_missing_id_list_is_a_shape_failure(self):
        fixture = baseline()
        del fixture["failing"]
        self.assert_shape_failure(fixture, "baseline.failing is missing")

    def test_an_unsorted_id_list_is_named_as_corruption(self):
        # Sorted unique is the writer's invariant (byte-stable output);
        # its violation is corruption, not content.
        fixture = baseline(failing=ids_of(["One", "Two"]))
        fixture["failing"].reverse()
        self.assert_shape_failure(fixture, "sorted unique")

    def test_a_duplicated_id_is_named_as_corruption(self):
        fixture = baseline(failing=ids_of(["One"]))
        fixture["failing"] = fixture["failing"] * 2
        self.assert_shape_failure(fixture, "sorted unique")

    def test_an_id_without_the_separator_is_a_shape_failure(self):
        fixture = baseline(failing=["features/tck/a.feature One"])
        self.assert_shape_failure(fixture, "is not a list of")

    def test_an_id_whose_name_carries_the_separator_is_accepted(self):
        # The opaque-id rule's baseline side: a name containing '::' is
        # legal prose, so the id parses (the corruption stance rejects
        # separator-less garbage, not corpus-shaped names).
        parsed = parse_baseline(
            baseline(failing=["features/tck/a.feature::map :: projection"])
        )
        self.assertEqual(
            parsed.failing, {"features/tck/a.feature::map :: projection"}
        )

    def test_an_id_with_an_empty_name_part_is_a_shape_failure(self):
        fixture = baseline(failing=["features/tck/a.feature::"])
        self.assert_shape_failure(fixture, "is not a list of")


class Semantics(unittest.TestCase):
    def gate(self, summary_features, base):
        """The verdict for a fixture pair — the one path the CLI takes,
        minus argument parsing."""
        ids = collect(summary(summary_features))
        return baseline_script.gate_verdict(ids, parse_baseline(base))

    def test_a_run_matching_the_baseline_is_green(self):
        verdict = self.gate(
            features(failures=[failure("One")]),
            baseline(failing=ids_of(["One"])),
        )
        self.assertFalse(verdict.red)
        self.assertEqual(verdict.new_count, 0)

    def test_a_new_failing_scenario_is_red_with_its_category(self):
        verdict = self.gate(
            features(
                failures=[failure("One"), failure("Two", "type-error")]
            ),
            baseline(failing=ids_of(["One"])),
        )
        self.assertTrue(verdict.red)
        self.assertEqual(verdict.new_count, 1)
        self.assertEqual(verdict.new_failed, {
            "features/tck/a.feature::Two": "type-error"
        })
        self.assertIn("type-error ×1", gate_script.render_verdict(verdict))

    def test_a_fix_n_break_n_swap_is_red(self):
        # The counts-only gate is blind to this; the id gate is not
        # (ADR-0661 §1) — one fixed, one broken, net zero.
        verdict = self.gate(
            features(failures=[failure("Two")]),
            baseline(failing=ids_of(["One"])),
        )
        self.assertTrue(verdict.red)
        self.assertEqual(verdict.new_count, 1)
        self.assertEqual(verdict.fixed, 1)

    def test_a_pure_improvement_is_green_and_counts_the_fixed(self):
        verdict = self.gate(
            features(failures=[failure("One")]),
            baseline(failing=ids_of(["One", "Two"])),
        )
        self.assertFalse(verdict.red)
        self.assertEqual(verdict.fixed, 1)
        self.assertIn("1 fixed on this PR", gate_script.render_verdict(verdict))

    def test_a_baseline_skip_excuses_a_now_failing_scenario(self):
        # A scenario that never ran on main cannot regress by running
        # and failing now (ADR-0661 §1).
        verdict = self.gate(
            features(failures=[failure("Admin")]),
            baseline(skip_ids=ids_of(["Admin"])),
        )
        self.assertFalse(verdict.red)

    def test_a_baseline_nondeterministic_failure_excuses_the_id(self):
        verdict = self.gate(
            features(failures=[failure("Wobbly")]),
            baseline(nondet_ids=ids_of(["Wobbly"])),
        )
        self.assertFalse(verdict.red)

    def test_a_baseline_step_crash_excuses_a_now_asserting_scenario(self):
        # The id was already not passing; moving from crash to assertion
        # is not a new failure.
        verdict = self.gate(
            features(failures=[failure("Crashed")]),
            baseline(errored_ids=ids_of(["Crashed"])),
        )
        self.assertFalse(verdict.red)

    def test_a_new_step_crash_is_red(self):
        verdict = self.gate(
            features(errored=[errored("Crashed", "engine-panic")]),
            baseline(failing=ids_of(["One"])),
        )
        self.assertTrue(verdict.red)
        self.assertEqual(verdict.new_errored, {
            "features/tck/a.feature::Crashed": "engine-panic"
        })
        self.assertIn("1 step crashes", verdict.counts_phrase)

    def test_a_marked_failure_never_enters_the_not_passing_set(self):
        # The PR's only new failure is `@fails`-marked: the expected
        # state, not a regression (ADR-0482 §3 carried into the gate).
        verdict = self.gate(
            features(failures=[failure("One", marked=True)]),
            baseline(),
        )
        self.assertFalse(verdict.red)

    def test_the_verdict_renders_the_baseline_provenance(self):
        text = gate_script.render_verdict(
            self.gate(features(failures=[failure("One")]),
                      baseline(failing=ids_of(["One"])))
        )
        self.assertIn(f"engine `{ENGINE_SHA[:7]}`", text)
        self.assertIn(f"harness `{HARNESS_SHA[:7]}`", text)
        self.assertIn("1 carried as not passing", text)




if __name__ == "__main__":
    unittest.main()
