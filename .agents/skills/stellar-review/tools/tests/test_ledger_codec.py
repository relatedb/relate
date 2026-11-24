"""Tests for ledger.py's comment-blob codec and the shared
complementary-split helper (moved from summary.py, REL8-1993)."""

import secrets
import unittest

import helpers  # noqa: F401  (puts tools/ on sys.path)
import ledger as ledger_mod  # noqa: E402


class TestLedgerBlob(unittest.TestCase):
    """The comment-embedded ledger blob's one codec: always the
    zlib+base64 "z:" arm, round-trippable, and budget-bounded."""

    def test_ledger_blob_is_always_compressed(self):
        # even a tiny ledger (filenames are attacker-controlled and the
        # plaintext arm would embed them verbatim)
        enc = ledger_mod.encode_ledger(ledger_mod.empty_ledger())
        self.assertTrue(enc.startswith("z:"))
        for marker in ("<!--", "-->"):
            self.assertNotIn(marker, enc)

    def test_compress_and_round_trip(self):
        led = {"version": 1, "rounds": 1, "last_head": "x",
               "units": {f"m{i}::f": "a" * 16 + ":" + "b" * 16 for i in range(500)},
               "findings": {}}
        enc = ledger_mod.encode_ledger(led)
        self.assertTrue(enc.startswith("z:"))
        self.assertEqual(ledger_mod.decode_ledger(enc), led)

    def test_budget_drops_unit_map_as_last_resort(self):
        # pathological: long uncompressible ids at huge unit counts
        led = {"version": 1, "rounds": 1, "last_head": "x",
               "units": {secrets.token_hex(40): secrets.token_hex(20)
                         for _ in range(4000)},
               "findings": {}}
        enc = ledger_mod.encode_ledger(led)
        dec = ledger_mod.decode_ledger(enc)
        self.assertLessEqual(len(enc), ledger_mod.LEDGER_BUDGET + 200)
        self.assertEqual(dec["units"], {})
        self.assertEqual(dec["dropped_units"], 4000)


class TestPartition(unittest.TestCase):
    """The shared complementary-split helper: both halves in one pass,
    order preserved."""

    def test_splits_both_halves(self):
        yes, no = ledger_mod.partition([1, 2, 3, 4], lambda x: x % 2 == 0)
        self.assertEqual(yes, [2, 4])
        self.assertEqual(no, [1, 3])

    def test_empty_input_yields_two_empty_halves(self):
        self.assertEqual(ledger_mod.partition([], bool), ([], []))


class TestLegacyPlaintextReader(unittest.TestCase):
    """The decoder keeps one named legacy-plaintext read; the encoder
    never emits it."""

    def test_plaintext_blob_decodes_but_is_never_emitted(self):
        legacy = '{"version":1,"rounds":2,"findings":{}}'
        self.assertEqual(ledger_mod.decode_ledger(legacy)["rounds"], 2)
        self.assertFalse(ledger_mod.encode_ledger({"version": 1}).startswith("{"))


class TestEndLine(unittest.TestCase):
    def test_zero_or_missing_end_means_the_start_line(self):
        self.assertEqual(ledger_mod.end_line([5, 9]), 9)
        self.assertEqual(ledger_mod.end_line([5, 0]), 5)
        self.assertEqual(ledger_mod.end_line([5, None]), 5)


if __name__ == "__main__":
    unittest.main()
