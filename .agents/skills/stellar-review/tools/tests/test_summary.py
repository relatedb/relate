"""Tests for summary.py — the round summary's renderer and its typed
outcome/disposition (extracted from post.py, REL8-1769)."""

import unittest

from helpers import FACTS, ledger_entry  # also puts tools/ on sys.path
import ledger as ledger_mod  # noqa: E402
import summary  # noqa: E402
from summary import Disposition, Outcome  # noqa: E402


class TestOutcome(unittest.TestCase):
    def test_partial_wins_and_only_pass_passes(self):
        self.assertIs(summary.round_outcome(False, False), Outcome.PASS)
        self.assertIs(summary.round_outcome(True, False), Outcome.FAIL)
        self.assertIs(summary.round_outcome(False, True), Outcome.PARTIAL)
        self.assertIs(summary.round_outcome(True, True), Outcome.PARTIAL)
        self.assertEqual([o.verdict for o in Outcome], ["pass", "fail", "fail"])


class TestBuildSummary(unittest.TestCase):
    """PR #329's mislabel: when the review API rejects inline comments
    (degraded), context findings must keep their own heading — never ride
    under "API rejected" — and the open count names its arithmetic."""

    def entry(self, finding_id):
        return ledger_entry(f"F-3-{finding_id}")

    def render(self, rejected, context_ids, anchored_ids):
        led = ledger_mod.empty_ledger()
        entries = [self.entry(i) for i in anchored_ids]
        disp = Disposition(
            new=entries, carried=[], resolved=[], waived=[], fail_new=entries,
            low_new=[], unanchored=[self.entry(i) for i in context_ids],
            anchored_carried=[], dropped_nits=0, dropped_scope=0, rejected=rejected)
        return summary.build_summary(FACTS, None, 3, "deadbeef", Outcome.FAIL, led, disp)

    def ids(self, ids):
        """The rejected-inline id set for the listed findings. The id is
        minted in `entry` alone; this keys off that one mint."""
        return {self.entry(i)["id"] for i in ids}

    def test_degraded_keeps_context_section_separate(self):
        out = self.render(self.ids([1]), context_ids=[2, 3], anchored_ids=[1])
        self.assertIn("**Findings the review API rejected inline for**", out)
        self.assertIn("**Findings on context units**", out)
        # both sections present, each with exactly its own findings
        rejected = out.split("**Findings the review API rejected inline for**")[1]
        rejected = rejected.split("**Findings on context units**")[0]
        self.assertEqual(rejected.count("F-3-1"), 1, rejected)
        context = out.split("**Findings on context units**")[1]
        self.assertEqual(context.count("F-3-2"), 1, context)
        self.assertEqual(context.count("F-3-3"), 1, context)

    def test_degraded_never_mislabels_a_no_anchor_finding(self):
        # A finding with no diff anchor was never attempted inline, so a
        # degraded round must not file it under "rejected inline" — only
        # the attempted-and-refused ones are in the rejected set.
        out = self.render(self.ids([1]), context_ids=[], anchored_ids=[1, 2])
        rejected = out.split("**Findings the review API rejected inline for**")[1]
        self.assertEqual(rejected.count("F-3-1"), 1, rejected)
        self.assertEqual(rejected.count("F-3-2"), 0, rejected)
        new = out.split("**New findings**")[1].split(
            "**Findings the review API rejected inline for**")[0]
        self.assertEqual(new.count("F-3-2"), 1, new)

    def test_open_count_names_arithmetic(self):
        out = self.render(set(), context_ids=[2], anchored_ids=[1])
        self.assertIn("1 issue(s) open (1 + 0 anchored new (fail/low) + 0 carried)", out)

    def test_open_any_and_open_gating_are_derived_once(self):
        e, c = self.entry(1), self.entry(2)
        base = dict(new=[], carried=[], resolved=[], waived=[], fail_new=[], low_new=[],
                    unanchored=[], anchored_carried=[], dropped_nits=0,
                    dropped_scope=0, rejected=set())
        ctx_only = Disposition(**dict(base, unanchored=[e]))
        self.assertTrue(ctx_only.open_any)
        self.assertEqual(ctx_only.open_gating, 0)
        carried = Disposition(**dict(base, carried=[c], anchored_carried=[c]))
        self.assertTrue(carried.open_any)
        self.assertEqual(carried.open_gating, 1)
        self.assertFalse(Disposition(**base).open_any)

    def test_carried_lines_hide_remedy_and_partial_has_its_own_line(self):
        c = self.entry(1)
        d = Disposition(new=[], carried=[c], resolved=[], waived=[], fail_new=[],
                        low_new=[], unanchored=[], anchored_carried=[c],
                        dropped_nits=0, dropped_scope=0, rejected=set())
        out = summary.build_summary(
            FACTS, None, 3, "deadbeef", Outcome.PARTIAL, ledger_mod.empty_ledger(), d)
        self.assertIn("Still open from earlier rounds", out)
        self.assertNotIn("_Fix:_", out)
        self.assertIn("partial round", out)
        self.assertIn("<!-- opencode-verdict: fail -->", out)

    def test_pass_outcome_with_nothing_open(self):
        d = Disposition(new=[], carried=[], resolved=[], waived=[], fail_new=[],
                        low_new=[], unanchored=[], anchored_carried=[],
                        dropped_nits=0, dropped_scope=0, rejected=set())
        out = summary.build_summary(
            FACTS, None, 3, "deadbeef", Outcome.PASS, ledger_mod.empty_ledger(), d)
        self.assertIn("No open issues.", out)
        self.assertIn("<!-- opencode-verdict: pass -->", out)


if __name__ == "__main__":
    unittest.main()
