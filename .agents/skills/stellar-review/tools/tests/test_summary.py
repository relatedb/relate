"""Tests for summary.py — the round summary's renderer, the ledger blob
codec it carries, and the shared complementary-split helper (all
extracted from post.py, REL8-1769)."""

import secrets
import unittest

from helpers import FACTS  # also puts tools/ on sys.path
import ledger as ledger_mod  # noqa: E402
import summary  # noqa: E402


class TestLedgerBlob(unittest.TestCase):
    """The comment-embedded ledger blob's one codec: always the
    zlib+base64 "z:" arm, round-trippable, and budget-bounded."""

    def test_ledger_blob_is_always_compressed(self):
        # even a tiny ledger (filenames are attacker-controlled and the
        # plaintext arm would embed them verbatim)
        enc = summary.encode_ledger(ledger_mod.empty_ledger())
        self.assertTrue(enc.startswith("z:"))
        for marker in ("<!--", "-->"):
            self.assertNotIn(marker, enc)

    def test_compress_and_round_trip(self):
        led = {"version": 1, "rounds": 1, "last_head": "x",
               "units": {f"m{i}::f": "a" * 16 + ":" + "b" * 16 for i in range(500)},
               "findings": {}}
        enc = summary.encode_ledger(led)
        self.assertTrue(enc.startswith("z:"))
        self.assertEqual(summary.decode_ledger(enc), led)

    def test_budget_drops_unit_map_as_last_resort(self):
        # pathological: long uncompressible ids at huge unit counts
        led = {"version": 1, "rounds": 1, "last_head": "x",
               "units": {secrets.token_hex(40): secrets.token_hex(20)
                         for _ in range(4000)},
               "findings": {}}
        enc = summary.encode_ledger(led)
        dec = summary.decode_ledger(enc)
        self.assertLessEqual(len(enc), summary.LEDGER_BUDGET + 200)
        self.assertEqual(dec["units"], {})
        self.assertEqual(dec["dropped_units"], 4000)


class TestPartition(unittest.TestCase):
    """The shared complementary-split helper: both halves in one pass,
    order preserved."""

    def test_splits_both_halves(self):
        yes, no = summary.partition([1, 2, 3, 4], lambda x: x % 2 == 0)
        self.assertEqual(yes, [2, 4])
        self.assertEqual(no, [1, 3])

    def test_empty_input_yields_two_empty_halves(self):
        self.assertEqual(summary.partition([], bool), ([], []))


class TestBuildSummary(unittest.TestCase):
    """PR #329's mislabel: when the review API rejects inline comments
    (degraded), context findings must keep their own heading — never ride
    under "API rejected" — and the open count names its arithmetic."""

    def entry(self, finding_id):
        return {"id": f"F-3-{finding_id}", "severity": "medium", "area": "boundaries",
                "checklist_id": "std-5", "path": "src/lib.rs", "lines": [5, 9],
                "summary": "s", "remedy": "r"}

    def render(self, rejected, context_ids, anchored_ids):
        led = ledger_mod.empty_ledger()
        entries = [self.entry(i) for i in anchored_ids]
        disp = {"fail_new": entries, "low_new": [],
                "unanchored": [self.entry(i) for i in context_ids],
                "carried": [], "anchored_carried": [],
                "resolved": [], "waived": [],
                "dropped_nits": 0, "dropped_scope": 0,
                "rejected": rejected,
                "timeout_partial": False}
        return summary.build_summary(FACTS, None, 3, "deadbeef", "fail", led, disp)

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


if __name__ == "__main__":
    unittest.main()
