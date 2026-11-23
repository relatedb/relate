"""Unit-test the run-side scenario-id contract
(`scripts/spec_harness_ids.py`, ADR-0661 §1): the `features` shape walk
driven by the one member table, the opaque id grammar (a scenario name
may carry the `::` separator — corpus names are prose), the `@fails`
exclusion carried into the gate, and the category rollup's order.

The shared fixtures live in `spec_harness_fixtures.py`.
"""

import unittest

from spec_harness_fixtures import (
    assert_parse_failure,
    collect,
    errored,
    features,
    failure,
    ids_script,
    nondet,
    skip,
    summary,
)


class FeatureShape(unittest.TestCase):
    def assert_shape_failure(self, data, needle):
        assert_parse_failure(
            ids_script.ScenarioIds.from_summary, data, needle
        )

    def test_a_contract_honoring_features_section_collects(self):
        ids = collect(
            summary(
                features(
                    failures=[failure("One"), failure("Two", marked=True)],
                    nondeterministic=[nondet("Wobbly")],
                    skips=[skip("Admin")],
                    errored=[errored("Crashed")],
                )
            )
        )
        # The @fails-marked failure is the expected state, never a
        # regression: it stays out of the not-passing set (ADR-0482 §3).
        self.assertEqual(
            ids.failing, {"features/tck/a.feature::One": "result-mismatch"}
        )
        self.assertEqual(
            ids.errored, {"features/tck/a.feature::Crashed": ""}
        )
        self.assertEqual(
            ids.nondeterministic, {"features/tck/a.feature::Wobbly"}
        )
        self.assertEqual(ids.skipped, {"features/tck/a.feature::Admin"})
        self.assertEqual(
            ids.not_passing,
            {
                "features/tck/a.feature::One",
                "features/tck/a.feature::Crashed",
            },
        )

    def test_a_missing_features_section_is_a_shape_failure(self):
        fixture = summary()
        del fixture["features"]
        self.assert_shape_failure(fixture, "features is missing")

    def test_a_non_object_features_section_is_a_shape_failure(self):
        self.assert_shape_failure(
            summary(features=None) | {"features": []}, "features"
        )

    def test_a_non_object_feature_entry_is_a_shape_failure(self):
        self.assert_shape_failure(
            summary({"features/tck/a.feature": []}),
            "not an object",
        )

    def test_a_missing_failures_member_is_a_writer_change(self):
        # The harness always writes the three lists; absence is not an
        # empty run but a summary shape change, named loudly.
        fixture = summary(features())
        del fixture["features"]["features/tck/a.feature"]["failures"]
        self.assert_shape_failure(fixture, "failures is missing")

    def test_a_non_list_member_is_a_shape_failure(self):
        fixture = summary(features())
        fixture["features"]["features/tck/a.feature"]["skips"] = {}
        self.assert_shape_failure(fixture, "skips is not a list")

    def test_an_absent_errored_member_is_the_older_harness(self):
        # `errored` is the member the per-scenario error listing added:
        # an older harness omits it and the gate reads empty.
        fixture = summary(features(errored=[errored("Crashed")]))
        del fixture["features"]["features/tck/a.feature"]["errored"]
        ids, diags = ids_script.ScenarioIds.from_summary(fixture)
        self.assertEqual(diags, [])
        self.assertEqual(ids.errored, {})

    def test_a_scenario_name_containing_the_separator_is_a_legal_id(self):
        # Corpus scenario names are prose that describes syntax — '::'
        # is Cypher syntax — and the id is opaque to every consumer, so
        # a name may carry the separator: it round-trips into the
        # not-passing set like any other (the first real bootstrap run
        # hit this: a failing scenario whose name contains '::').
        ids = collect(
            summary(features(failures=[failure("path :: create :: node")]))
        )
        self.assertEqual(
            ids.not_passing,
            {"features/tck/a.feature::path :: create :: node"},
        )

    def test_a_non_string_scenario_name_is_a_shape_failure(self):
        item = failure("One")
        item["scenario"] = 7
        self.assert_shape_failure(
            summary(features(failures=[item])), "scenario name"
        )

    def test_an_empty_scenario_name_is_a_shape_failure(self):
        item = failure("One")
        item["scenario"] = ""
        self.assert_shape_failure(
            summary(features(failures=[item])), "scenario name"
        )

    def test_a_non_true_fails_marking_is_malformed(self):
        item = failure("One")
        item["neo4j_fails_tag"] = "yes"
        self.assert_shape_failure(
            summary(features(failures=[item])), "not boolean true"
        )

    def test_a_malformed_failure_category_is_a_shape_failure(self):
        self.assert_shape_failure(
            summary(features(failures=[failure("One", "bad name!")])),
            "malformed category",
        )

    def test_a_genuine_failure_without_a_category_is_a_shape_failure(self):
        item = failure("One")
        del item["category"]
        self.assert_shape_failure(
            summary(features(failures=[item])), "no category"
        )

    def test_a_non_object_list_member_is_a_shape_failure(self):
        self.assert_shape_failure(
            summary(features(skips=["Admin"])), "skips entry"
        )
        self.assert_shape_failure(
            summary(features(nondeterministic=["Wobbly"])),
            "nondeterministic entry",
        )

    def test_an_errored_entry_may_carry_no_category(self):
        # The error listing need not classify: an uncategorized entry
        # reports under the verdict's step-crash count.
        ids = collect(summary(features(errored=[errored("Crashed")])))
        self.assertEqual(
            ids.errored, {"features/tck/a.feature::Crashed": ""}
        )




if __name__ == "__main__":
    unittest.main()
