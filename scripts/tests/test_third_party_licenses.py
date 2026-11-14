"""render() of scripts/third-party-licenses.py: deterministic, CR-free,
one section per license id, text once, every crate listed once."""
import unittest

from script_loader import ROOT, load

tpl = load("third_party_licenses", ROOT / "scripts" / "third-party-licenses.py")

GROUPS = {
    ("MIT", "Copyright (c) A\r\nPermission is hereby granted"): {
        ("zeta", "1.0.0"): "https://example.test/zeta",
        ("alpha", "2.1.0"): "",
    },
    ("Apache-2.0", "Apache License\nVersion 2.0"): {("beta", "0.3.0"): "https://example.test/beta"},
    ("MIT", "Copyright (c) B"): {("gamma", "0.1.0"): ""},
}


class RenderTest(unittest.TestCase):
    def test_counts_and_sections(self):
        out = tpl.render(GROUPS)
        self.assertIn("4 crates under 2 licenses.", out)
        self.assertEqual(out.count("\n## "), 2)
        self.assertTrue(out.endswith("```\n"))

    def test_one_text_per_license_and_notices_kept(self):
        out = tpl.render(GROUPS)
        self.assertEqual(out.count("## MIT"), 1)
        self.assertEqual(out.count("Permission is hereby granted"), 1)
        self.assertIn("  - Copyright (c) A\n", out)
        self.assertIn("  - Copyright (c) B\n", out)

    def test_no_carriage_returns(self):
        self.assertNotIn("\r", tpl.render(GROUPS))

    def test_deterministic_regardless_of_insertion_order(self):
        reordered = dict(reversed(list(GROUPS.items())))
        self.assertEqual(tpl.render(GROUPS), tpl.render(reordered))

    def test_crates_sorted_and_repository_optional(self):
        out = tpl.render(GROUPS)
        self.assertLess(out.index("- alpha 2.1.0\n"), out.index("- zeta 1.0.0 (https://example.test/zeta)"))


class NoticeOrderTest(unittest.TestCase):
    def test_notices_sorted_regardless_of_text_order(self):
        a = {("MIT", "Copyright (c) B\nx"): {("c", "1.0.0"): ""}, ("MIT", "Copyright (c) A\ny"): {("c", "1.0.0"): ""}}
        self.assertEqual(tpl.render(a), tpl.render(dict(reversed(list(a.items())))))
        self.assertLess(tpl.render(a).index("Copyright (c) A"), tpl.render(a).index("Copyright (c) B"))


class NoticePolicyTest(unittest.TestCase):
    def test_apache_clause_is_not_a_notice(self):
        self.assertEqual(tpl.copyright_lines("(c) You must retain, in the Source form"), [])
        self.assertEqual(tpl.copyright_lines("(c) 2020 Jane"), ["(c) 2020 Jane"])


class ListParityTest(unittest.TestCase):
    def test_committed_lists_agree_and_are_nonempty(self):
        self.assertTrue(tpl.license_list("about.toml", "accepted"))
        tpl.check_lists_agree()


if __name__ == "__main__":
    unittest.main()
