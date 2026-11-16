"""Tests for scripts/check_deny_expiry.py.

The fixtures are hermetic deny.toml shapes with a fixed "today", so the
boundary is pinned: an entry dated today is current, one dated yesterday
is the re-triage prompt. Entry discovery is structural (tomllib's
[advisories].ignore list), so the suite pins every entry form the policy
must reach — the repo's inline entries, single-quoted ids,
[[advisories.ignore]] table entries — plus the two evasions the
structural parse closes: expiry-like text inside a reason string, and a
valid-TOML entry shape the check cannot see (which fails loudly, never
rides). Two gates read the repo's real deny.toml: every live entry is
current, and a copy with one flipped expiry date is reported — the
failure the weekly cargo-deny workflow exists to surface.
"""

import tempfile
import unittest
from contextlib import redirect_stderr
from datetime import date, timedelta
from io import StringIO
from pathlib import Path

from script_loader import ROOT, load

expiry = load("check_deny_expiry", ROOT / "scripts" / "check_deny_expiry.py")

TODAY = date(2026, 1, 15)

# The deny.toml ignore-entry shapes: inline entries (the repo file's
# form, both quote styles) with the expiry riding as a comment on the
# entry's id line, and [[advisories.ignore]] table entries carrying the
# same comment on their id line.
CURRENT_TOML = """\
[advisories]
ignore = [
    { id = "RUSTSEC-2025-0001", reason = "fixture: expires today" }, # expiry: 2026-01-15
    { id = "RUSTSEC-2025-0002", reason = "fixture: expires later" }, # expiry: 2026-06-01
]
"""

SINGLE_QUOTED_TOML = """\
[advisories]
ignore = [
    { id = 'RUSTSEC-2025-0004', reason = 'fixture: single-quoted entry' }, # expiry: 2026-01-15
]
"""

TABLE_FORM_TOML = """\
[advisories]
[[advisories.ignore]]
id = "RUSTSEC-2025-0005" # expiry: 2026-01-15
reason = "fixture: table-form entry"
"""


def inline_toml(entry: str) -> str:
    """One hermetic deny.toml around a single inline ignore entry."""
    return f"[advisories]\nignore = [\n    {entry}\n]\n"


class ParseTests(unittest.TestCase):
    """One triple per ignore entry, every entry form included; nothing
    else becomes one."""

    def test_entries_carry_line_id_and_expiry(self) -> None:
        self.assertEqual(
            expiry.parse_entries(CURRENT_TOML),
            [
                (3, "RUSTSEC-2025-0001", "2026-01-15"),
                (4, "RUSTSEC-2025-0002", "2026-06-01"),
            ],
        )

    def test_single_quoted_entries_are_entries(self) -> None:
        # the round-2 bypass: a regex over `id = "…"` lines never saw a
        # single-quoted entry, so it rode with no expiry enforced at all
        self.assertEqual(
            expiry.parse_entries(SINGLE_QUOTED_TOML),
            [(3, "RUSTSEC-2025-0004", "2026-01-15")],
        )

    def test_table_form_entries_are_entries(self) -> None:
        self.assertEqual(
            expiry.parse_entries(TABLE_FORM_TOML),
            [(3, "RUSTSEC-2025-0005", "2026-01-15")],
        )

    def test_a_missing_expiry_comment_parses_as_empty(self) -> None:
        entries = expiry.parse_entries(
            inline_toml('{ id = "RUSTSEC-2025-0003", reason = "fixture: no expiry" },'))
        self.assertEqual(entries, [(3, "RUSTSEC-2025-0003", "")])

    def test_a_file_without_ignore_entries_has_none(self) -> None:
        self.assertEqual(expiry.parse_entries("[bans]\n\n# expiry: 2020-01-01\n"), [])

    def test_an_id_assignment_inside_a_reason_string_is_not_an_entry(self) -> None:
        # the structural list names the entries; a quoted `id = …`
        # inside another entry's reason can never locate (or hide) one
        text = inline_toml(
            '{ id = "RUSTSEC-2025-0001", '
            'reason = "see also id = \\"RUSTSEC-2025-0002\\" in the old notes" }, '
            '# expiry: 2026-01-15')
        self.assertEqual(expiry.parse_entries(text),
                         [(3, "RUSTSEC-2025-0001", "2026-01-15")])

    def test_an_entry_shape_the_check_cannot_see_raises(self) -> None:
        # valid TOML, invisible to the check: never ridden — the raise
        # is what check() turns into the run's loud failure
        shapes = [
            ('[advisories]\nignore = "not a list"\n', "ignore not a list"),
            ('[advisories]\nignore = [ { reason = "no id" } ]\n', "entry without id"),
            ('[advisories]\nignore = [ { id = 123, reason = "r" } ]\n', "non-string id"),
            ('[advisories]\nignore = [ { "id" = "RUSTSEC-X", reason = "r" } ]\n',
             "quoted key"),
        ]
        for text, label in shapes:
            with self.subTest(shape=label):
                with self.assertRaises(expiry.UnseenShapeError):
                    expiry.parse_entries(text)


class PastDueTests(unittest.TestCase):
    """The boundary (today passes, yesterday fails), the two shape
    failures, and the reason-string forgery the comment region closes."""

    def entry(self, expiry_comment: str) -> list[tuple[int, str, str]]:
        return expiry.parse_entries(inline_toml(
            f'{{ id = "RUSTSEC-2025-0001", reason = "fixture" }}, {expiry_comment}'))

    def test_today_and_future_entries_are_current(self) -> None:
        self.assertEqual(expiry.past_due(self.entry("# expiry: 2026-01-15"), TODAY), [])
        self.assertEqual(expiry.past_due(self.entry("# expiry: 2027-01-01"), TODAY), [])

    def test_a_past_due_entry_names_itself_and_its_date(self) -> None:
        failures = expiry.past_due(self.entry("# expiry: 2026-01-14"), TODAY)
        self.assertEqual(len(failures), 1)
        self.assertEqual(failures[0][0], 3)
        self.assertEqual(failures[0][1], "RUSTSEC-2025-0001")
        self.assertIn("expired 2026-01-14", failures[0][2])

    def test_a_missing_expiry_comment_fails(self) -> None:
        failures = expiry.past_due(self.entry(""), TODAY)
        self.assertEqual(len(failures), 1)
        self.assertIn("no expiry comment", failures[0][2])

    def test_a_malformed_expiry_fails(self) -> None:
        for bad in ["# expiry: soon", "# expiry: 2026-1-5", "# expiry: 2026-13-40"]:
            with self.subTest(expiry=bad):
                failures = expiry.past_due(self.entry(bad), TODAY)
                self.assertEqual(len(failures), 1, failures)
                self.assertIn("not a", failures[0][2])

    def test_expiry_like_text_inside_the_reason_is_not_the_expiry(self) -> None:
        # the round-2 forgery, both directions: the expiry is read only
        # from the comment after the entry, never from inside a reason
        # string — a far-future expiry forged there cannot ride an
        # expired entry green, nor mask a missing comment
        forged = ('{ id = "RUSTSEC-2025-0001", '
                  'reason = "rides as # expiry: 2099-01-01 inside the reason" }')
        missing = expiry.past_due(expiry.parse_entries(inline_toml(f"{forged},")), TODAY)
        self.assertEqual(len(missing), 1)
        self.assertIn("no expiry comment", missing[0][2])
        expired = expiry.past_due(
            expiry.parse_entries(inline_toml(f"{forged}, # expiry: 2026-01-14")), TODAY)
        self.assertEqual(len(expired), 1)
        self.assertIn("expired 2026-01-14", expired[0][2])


class CheckTests(unittest.TestCase):
    """check() over a file, the CLI exit code the workflow reads, and
    the entry forms the round-2 re-review found riding unenforced."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.toml = Path(self._tmp.name) / "deny.toml"

    def test_a_current_file_reports_no_failures(self) -> None:
        self.toml.write_text(CURRENT_TOML, encoding="utf-8")
        self.assertEqual(expiry.check(self.toml, TODAY), [])

    def test_main_exits_0_on_today_and_1_on_a_flipped_date(self) -> None:
        # main reads the real clock (the workflow's contract): an entry
        # dated today is current, and the same entry flipped to
        # yesterday fails the run.
        today = expiry.today_utc()
        entry = '    {{ id = "RUSTSEC-2025-0001", reason = "fixture" }}, # expiry: {d}\n'
        self.toml.write_text(
            "[advisories]\nignore = [\n" + entry.format(d=today.isoformat()) + "]\n",
            encoding="utf-8",
        )
        self.assertEqual(expiry.main(["check_deny_expiry.py", str(self.toml)]), 0)
        self.toml.write_text(
            "[advisories]\nignore = [\n"
            + entry.format(d=(today - timedelta(days=1)).isoformat()) + "]\n",
            encoding="utf-8",
        )
        with redirect_stderr(StringIO()):
            self.assertEqual(expiry.main(["check_deny_expiry.py", str(self.toml)]), 1)

    def test_a_single_quoted_expired_entry_is_caught(self) -> None:
        # the round-2 bypass reproduced and closed: the single-quoted
        # entry was invisible to the old line regex and rode green with
        # no expiry enforced; the structural parse catches its expiry
        self.toml.write_text(
            SINGLE_QUOTED_TOML.replace("# expiry: 2026-01-15", "# expiry: 2026-01-14"),
            encoding="utf-8",
        )
        failures = expiry.check(self.toml, TODAY)
        self.assertEqual(len(failures), 1, failures)
        self.assertIn("expired 2026-01-14", failures[0][2])

    def test_a_table_form_expired_entry_is_caught(self) -> None:
        # same closure for the [[advisories.ignore]] form: an expired
        # table entry fails the run, not just the inline form
        self.toml.write_text(
            TABLE_FORM_TOML.replace("# expiry: 2026-01-15", "# expiry: 2026-01-14"),
            encoding="utf-8",
        )
        failures = expiry.check(self.toml, TODAY)
        self.assertEqual(len(failures), 1, failures)
        self.assertIn("expired 2026-01-14", failures[0][2])

    def test_an_unseen_shape_fails_loudly_through_the_cli(self) -> None:
        # a valid-TOML entry the check cannot tie to an id line is a
        # loud failure, never an unenforced ride
        self.toml.write_text(
            '[advisories]\nignore = [ { "id" = "RUSTSEC-X", reason = "r" } ]\n',
            encoding="utf-8",
        )
        with redirect_stderr(StringIO()) as err:
            self.assertEqual(expiry.main(["check_deny_expiry.py", str(self.toml)]), 1)
        self.assertIn("unseen shape", err.getvalue())
        self.assertIn("RUSTSEC-X", err.getvalue())

    def test_invalid_toml_fails_loudly_through_the_cli(self) -> None:
        self.toml.write_text("[advisories\nignore = [\n", encoding="utf-8")
        with redirect_stderr(StringIO()) as err:
            self.assertEqual(expiry.main(["check_deny_expiry.py", str(self.toml)]), 1)
        self.assertIn("unseen shape", err.getvalue())


class RepoGateTests(unittest.TestCase):
    """The two live gates: the repo file is current, and a flipped date is caught."""

    def test_every_repo_deny_toml_entry_is_current(self) -> None:
        self.assertEqual(expiry.check(ROOT / "deny.toml", expiry.today_utc()), [])

    def test_a_flipped_expiry_date_in_a_deny_toml_copy_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            flipped = Path(tmp) / "deny.toml"
            flipped.write_text(
                (ROOT / "deny.toml").read_text(encoding="utf-8").replace(
                    "# expiry: 2027-05-14",
                    f"# expiry: {(expiry.today_utc() - timedelta(days=1)).isoformat()}",
                    1,
                ),
                encoding="utf-8",
            )
            failures = expiry.check(flipped, expiry.today_utc())
            self.assertEqual(len(failures), 1, failures)
            self.assertIn("expired", failures[0][2])


if __name__ == "__main__":
    unittest.main()
