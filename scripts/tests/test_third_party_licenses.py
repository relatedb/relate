"""scripts/third-party-licenses.py: the one license table, the grouping step
and the pure notices formatter — deterministic, CR-free, one section per
license id, text once, every crate listed once."""
import re
import unittest

from script_loader import ROOT, load

tpl = load("third_party_licenses", ROOT / "scripts" / "third-party-licenses.py")


# The tool's one composition of the grouping step and the formatter, under
# the name the cases below use.
render = tpl.notices_body


# The committed SPDX text for MIT, read through the tool's own reader, plus a
# crate-specific notice line: the inputs are the real text shape, not a
# hand-rolled stand-in for it.
MIT_TEXT = tpl.standard_text("MIT")
NOTICED_MIT = MIT_TEXT + "\nCopyright (c) 2020 Example Authors\n"

GROUPS = {
    ("MIT", NOTICED_MIT): {
        ("zeta", "1.0.0"): "https://example.test/zeta",
        ("alpha", "2.1.0"): None,
    },
    ("Apache-2.0", "Apache License\nVersion 2.0"): {("beta", "0.3.0"): "https://example.test/beta"},
    ("MIT", NOTICED_MIT.replace("Example", "Other")): {("gamma", "0.1.0"): None},
}


class RenderTest(unittest.TestCase):
    def test_counts_and_sections(self):
        out = render(GROUPS)
        self.assertIn("4 crates under 2 licenses.", out)
        self.assertEqual(out.count("\n## "), 2)
        self.assertTrue(out.endswith("```\n"))

    def test_one_text_per_license_and_notices_kept(self):
        out = render(GROUPS)
        self.assertEqual(out.count("## MIT"), 1)
        # the committed standard text is shown once, not the crates' copies
        self.assertEqual(out.count("Permission is hereby granted"), 1)
        self.assertIn("  - Copyright (c) 2020 Example Authors\n", out)
        self.assertIn("  - Copyright (c) 2020 Other Authors\n", out)

    def test_no_carriage_returns(self):
        self.assertNotIn("\r", render(GROUPS))
        # a CRLF text of a license with no committed standard text is
        # normalized before it reaches the output
        out = render({("no-such-license", "a\r\ntext\r\n"): {("c", "1.0.0"): None}})
        self.assertNotIn("\r", out)
        self.assertIn("a\ntext", out)

    def test_deterministic_regardless_of_insertion_order(self):
        reordered = dict(reversed(list(GROUPS.items())))
        self.assertEqual(render(GROUPS), render(reordered))

    def test_crates_sorted_and_repository_optional(self):
        out = render(GROUPS)
        self.assertLess(out.index("- alpha 2.1.0\n"), out.index("- zeta 1.0.0 (https://example.test/zeta)"))


class NoticeOrderTest(unittest.TestCase):
    def test_notices_sorted_regardless_of_text_order(self):
        a = {
            ("MIT", MIT_TEXT + "\nCopyright (c) B\n"): {("c", "1.0.0"): None},
            ("MIT", MIT_TEXT + "\nCopyright (c) A\n"): {("c", "1.0.0"): None},
        }
        self.assertEqual(render(a), render(dict(reversed(list(a.items())))))
        self.assertLess(render(a).index("Copyright (c) A"), render(a).index("Copyright (c) B"))

    def test_notices_dedupe_ignores_whitespace(self):
        # the same notice spelled with different whitespace is one notice,
        # and the surviving spelling does not depend on the text order
        a = {
            ("MIT", MIT_TEXT + "\nCopyright  (c) 2020  Example\n"): {("c", "1.0.0"): None},
            ("MIT", MIT_TEXT + "\nCopyright (c) 2020 Example\n"): {("c", "1.0.0"): None},
        }
        self.assertEqual(render(a), render(dict(reversed(list(a.items())))))
        self.assertEqual(render(a).count("  - Copyright"), 1)


class NoticePolicyTest(unittest.TestCase):
    def test_apache_clause_is_not_a_notice(self):
        self.assertEqual(tpl.copyright_notices("(c) You must retain, in the Source form"), set())
        self.assertEqual(tpl.copyright_notices("(c) 2020 Jane"), {"(c) 2020 Jane"})

    def test_one_notice_is_one_entry_however_spelled(self):
        # the squashed spelling is both the identity and the text shown
        self.assertEqual(
            tpl.copyright_notices("Copyright  (c) 2020  Example\nCopyright (c) 2020 Example\n"),
            {"Copyright (c) 2020 Example"},
        )


class StandardTextTest(unittest.TestCase):
    def test_no_standard_text_is_one_value(self):
        # a compound expression has no single standard text, and neither has
        # a license the committed list does not carry
        self.assertIsNone(tpl.standard_text("Apache-2.0 WITH LLVM-exception"))
        self.assertIsNone(tpl.standard_text("no-such-license"))

    def test_canonical_text_fallback_carries_the_crates_own_text(self):
        lic_id = "Apache-2.0 WITH LLVM-exception"
        out = render({(lic_id, "the crates' own text\n"): {("c", "1.0.0"): None}})
        self.assertIn("## Apache-2.0 WITH LLVM-exception", out)
        self.assertIn("the crates' own text", out)


class OneLicenseTableTest(unittest.TestCase):
    def test_deny_toml_is_the_one_license_table(self):
        allow = tpl.deny_allow_list()
        self.assertTrue(allow)
        # about.toml carries no second copy of the vocabulary
        self.assertIsNone(re.search(r"^accepted\s*=", tpl.ABOUT.read_text(), re.M))

    def test_about_config_derives_the_accepted_list(self):
        config = tpl.about_config()
        for lic in tpl.deny_allow_list():
            self.assertIn(f'"{lic}"', config)
        # about.toml's own settings ride along
        self.assertIn("ignore-dev-dependencies = true", config)


if __name__ == "__main__":
    unittest.main()
