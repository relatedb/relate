#!/usr/bin/env python3
"""Direct unit tests for scripts/changes_collect.py — the .changes/
fragment collector behind the release cut (ADR-0627). The fragment
grammar (names, sections, bullets), the collection order (ascending PR
number), the splice under [Unreleased] (canonical section order, the
existing body passing through verbatim), the consumed-file deletion, and
the refusal classes — every finding named in one CollectError through
the CLI's single reporting site.

Run: python3 scripts/tests/test_changes_collect.py
"""

from __future__ import annotations

import contextlib
import io
import tempfile
import unittest
from pathlib import Path

from script_loader import ROOT, load

collector = load("changes_collect", ROOT / "scripts" / "changes_collect.py")

CHANGELOG = """# Changelog

## [Unreleased]

## [1.0.0] — 2026-11-08

### Added

- First release.

[Unreleased]: https://github.com/relatedb/relate/compare/v1.0.0...HEAD

[1.0.0]: https://github.com/relatedb/relate/releases/tag/v1.0.0
"""


def write(tmp: Path, name: str, text: str) -> Path:
    path = tmp / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    return path


class Sandbox(unittest.TestCase):
    """A temp directory with the canonical changelog and a .changes/
    pool, rebuilt per test."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.changelog = write(self.tmp, "CHANGELOG.md", CHANGELOG)
        self.changes = self.tmp / ".changes"
        self.changes.mkdir()

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def fragment(self, name: str, text: str) -> Path:
        return write(self.changes, name, text)


class Discovery(Sandbox):
    def test_readme_and_dotfiles_are_not_fragments(self) -> None:
        self.fragment("README.md", "# the format\n")
        self.fragment(".DS_Store", "junk")
        self.fragment("1-one.md", "### Added\n\n- One.\n")
        fragments, findings = collector.discover(self.changes)
        self.assertEqual(findings, [])
        self.assertEqual([p.name for p in fragments], ["1-one.md"])

    def test_ascending_pr_number_not_lexicographic(self) -> None:
        for name in ("999-late.md", "2-early.md", "1000-newer.md"):
            self.fragment(name, "### Added\n\n- x\n")
        fragments, findings = collector.discover(self.changes)
        self.assertEqual(findings, [])
        self.assertEqual(
            [p.name for p in fragments],
            ["2-early.md", "999-late.md", "1000-newer.md"],
        )

    def test_missing_directory_is_a_finding(self) -> None:
        fragments, findings = collector.discover(self.tmp / "nowhere")
        self.assertEqual(fragments, [])
        self.assertEqual(len(findings), 1, findings)
        self.assertIn("no fragment directory", findings[0])

    def test_every_pool_shape_offender_is_named(self) -> None:
        # One pool, three offenders of three causes, one valid fragment:
        # every offender is named in one findings list — never one
        # offender per run — and the valid fragment still pools.
        self.fragment("1-one.md", "### Added\n\n- One.\n")
        self.fragment("2-two.txt", "### Added\n\n- No.\n")
        (self.changes / "3-nested.md").mkdir()
        self.fragment("four-change.md", "### Added\n\n- No.\n")
        fragments, findings = collector.discover(self.changes)
        self.assertEqual([p.name for p in fragments], ["1-one.md"])
        self.assertEqual(len(findings), 3, findings)
        self.assertTrue(
            any("2-two.txt" in f and "not a fragment" in f for f in findings),
            findings,
        )
        self.assertTrue(
            any("3-nested.md" in f and "directory" in f for f in findings),
            findings,
        )
        self.assertTrue(
            any(
                "four-change.md" in f and "<PR number>-<slug>.md" in f
                for f in findings
            ),
            findings,
        )


class FragmentGrammar(Sandbox):
    def test_bullets_and_wrapped_continuations_parse(self) -> None:
        sections = collector.parse_sections(
            ["### Changed\n", "\n", "- A flag\n", "  now works\n"],
            "f"
        )
        self.assertEqual(sections, {"Changed": [["- A flag", "  now works"]]})

    def test_blank_separated_bullets_are_separate_entries(self) -> None:
        sections = collector.parse_sections(
            ["### Added\n", "\n", "- One.\n", "\n", "- Two.\n"],
            "f"
        )
        self.assertEqual(sections, {"Added": [["- One."], ["- Two."]]})

    def test_unknown_section_is_a_refusal(self) -> None:
        with self.assertRaises(collector.CollectError) as ctx:
            collector.parse_sections(
                ["### Additions\n", "\n", "- One.\n"], "f"
            )
        self.assertIn("unknown section heading", str(ctx.exception))

    def test_a_repeated_section_is_a_refusal(self) -> None:
        with self.assertRaises(collector.CollectError) as ctx:
            collector.parse_sections(
                ["### Added\n", "\n", "- One.\n", "### Added\n", "\n", "- Two.\n"],
                "f",
            )
        self.assertIn("appears twice", str(ctx.exception))

    def test_an_empty_section_is_a_refusal(self) -> None:
        with self.assertRaises(collector.CollectError) as ctx:
            collector.parse_sections(
                ["### Added\n", "\n", "### Changed\n", "\n", "- One.\n"],
                "f",
            )
        self.assertIn("'Added' is empty", str(ctx.exception))

    def test_content_before_the_first_heading_is_a_refusal(self) -> None:
        with self.assertRaises(collector.CollectError) as ctx:
            collector.parse_sections(
                ["A note.\n", "### Added\n", "\n", "- One.\n"],
                "f",
            )
        self.assertIn("before the first", str(ctx.exception))

    def test_an_unindented_non_bullet_line_is_a_refusal(self) -> None:
        with self.assertRaises(collector.CollectError) as ctx:
            collector.parse_sections(
                ["### Added\n", "\n", "One.\n"], "f"
            )
        self.assertIn("neither a '- ' bullet", str(ctx.exception))

    def test_an_indented_line_with_no_bullet_is_a_refusal(self) -> None:
        with self.assertRaises(collector.CollectError) as ctx:
            collector.parse_sections(
                ["### Added\n", "\n", "  stray\n"], "f"
            )
        self.assertIn("no bullet to continue", str(ctx.exception))


class Collect(Sandbox):
    def test_fragments_splice_under_unreleased_in_order(self) -> None:
        self.fragment(
            "3-grammar.md",
            "### Changed\n\n- Parameter names may be backticked.\n",
        )
        self.fragment(
            "2-flag.md",
            "### Added\n\n- A flag.\n\n### Notes\n\n- Internal note.\n",
        )
        self.assertEqual(
            collector.collect(self.changelog, self.changes), 0
        )
        text = self.changelog.read_text()
        # Canonical section order, entries in ascending PR order, the
        # released section and the ref block untouched below.
        self.assertEqual(
            text.splitlines()[4:14],
            [
                "### Added",
                "",
                "- A flag.",
                "",
                "### Changed",
                "",
                "- Parameter names may be backticked.",
                "",
                "### Notes",
                "",
            ],
        )
        self.assertIn("- Internal note.", text)
        self.assertIn("## [1.0.0] — 2026-11-08", text)
        self.assertIn(
            "[Unreleased]: https://github.com/relatedb/relate/compare/v1.0.0...HEAD",
            text,
        )
        # The consumed fragments are deleted from the pool.
        self.assertEqual([p.name for p in self.changes.iterdir()], [])

    def test_readme_survives_the_collection(self) -> None:
        self.fragment("README.md", "# the format\n")
        self.fragment("1-one.md", "### Added\n\n- One.\n")
        self.assertEqual(collector.collect(self.changelog, self.changes), 0)
        self.assertEqual(
            [p.name for p in self.changes.iterdir()], ["README.md"]
        )

    def test_entries_render_adjacent_within_a_section(self) -> None:
        self.fragment("1-a.md", "### Added\n\n- First.\n")
        self.fragment("2-b.md", "### Added\n\n- Second.\n")
        collector.collect(self.changelog, self.changes)
        text = self.changelog.read_text()
        self.assertIn("- First.\n- Second.\n", text)

    def test_existing_body_passes_through_and_fragments_append(self) -> None:
        changelog = CHANGELOG.replace(
            "## [Unreleased]\n",
            "## [Unreleased]\n\n### Added\n\n- Hand-filed entry.\n",
            1,
        )
        self.changelog.write_text(changelog)
        self.fragment("1-frag.md", "### Changed\n\n- Fragment entry.\n")
        self.assertEqual(collector.collect(self.changelog, self.changes), 0)
        text = self.changelog.read_text()
        self.assertLess(text.index("Hand-filed"), text.index("Fragment entry"))
        self.assertLess(text.index("Fragment entry"), text.index("## [1.0.0]"))

    def test_fragment_sections_colliding_with_the_body_accumulate(self) -> None:
        # The transition window: the body already carries Added and
        # Changed sections, and two fragments collide on each. Every
        # collision is named — fragment and section — in one refusal.
        changelog = CHANGELOG.replace(
            "## [Unreleased]\n",
            "## [Unreleased]\n\n### Added\n\n- Hand-filed entry.\n\n"
            "### Changed\n\n- Also hand-filed.\n",
            1,
        )
        self.changelog.write_text(changelog)
        self.fragment("1-a.md", "### Added\n\n- Fragment entry.\n")
        self.fragment("2-b.md", "### Changed\n\n- Fragment entry.\n")
        with self.assertRaises(collector.CollectError) as ctx:
            collector.collect(self.changelog, self.changes)
        message = str(ctx.exception)
        self.assertIn("1-a.md", message)
        self.assertIn("'Added'", message)
        self.assertIn("2-b.md", message)
        self.assertIn("'Changed'", message)
        # Nothing was written, nothing consumed.
        self.assertEqual(self.changelog.read_text(), changelog)
        self.assertTrue((self.changes / "1-a.md").exists())
        self.assertTrue((self.changes / "2-b.md").exists())

    def test_first_release_body_shape_passes_through(self) -> None:
        # A first-release changelog carries no version section below
        # [Unreleased]; the body ends at the ref block, and a
        # non-canonical section heading in it is pre-existing content —
        # it passes through verbatim, fragments append below it.
        changelog = (
            "# Changelog\n\n## [Unreleased]\n\n### Initial release\n\n"
            "First release.\n\n"
            "[Unreleased]: https://github.com/relatedb/relate\n"
        )
        self.changelog.write_text(changelog)
        self.fragment("1-frag.md", "### Added\n\n- Fragment entry.\n")
        self.assertEqual(collector.collect(self.changelog, self.changes), 0)
        text = self.changelog.read_text()
        self.assertIn("First release.", text)
        self.assertLess(text.index("First release."), text.index("Fragment entry"))
        self.assertIn("[Unreleased]: https://github.com/relatedb/relate\n", text)

    def test_empty_pool_leaves_the_changelog_untouched(self) -> None:
        self.assertEqual(collector.collect(self.changelog, self.changes), 0)
        self.assertEqual(self.changelog.read_text(), CHANGELOG)

    def test_every_invalid_fragment_is_named(self) -> None:
        # The accumulation contract: two refused fragments, both named in
        # read_pool's findings — never one refusal per run.
        self.fragment("1-good.md", "### Added\n\n- One.\n")
        self.fragment("2-bad-section.md", "### Additions\n\n- No.\n")
        self.fragment("3-bad-line.md", "### Added\n\nNo bullet.\n")
        pool, findings = collector.read_pool(self.changes)
        self.assertEqual(len(findings), 2, findings)
        # The pool is one record per parseable fragment — the good one
        # carries its sections with it, never a parallel list.
        self.assertEqual([p.name for p, _ in pool], ["1-good.md"])
        self.assertEqual(pool[0][1], {"Added": [["- One."]]})
        self.assertTrue(
            any("2-bad-section.md" in f and "unknown section" in f for f in findings),
            findings,
        )
        self.assertTrue(
            any("3-bad-line.md" in f and "neither a '- ' bullet" in f for f in findings),
            findings,
        )
        # Collecting the same pool refuses through one channel, touching
        # nothing.
        with self.assertRaises(collector.CollectError) as ctx:
            collector.collect(self.changelog, self.changes)
        self.assertIn("2-bad-section.md", str(ctx.exception))
        self.assertIn("3-bad-line.md", str(ctx.exception))
        self.assertEqual(self.changelog.read_text(), CHANGELOG)
        self.assertTrue((self.changes / "1-good.md").exists())


class UnreleasedBody(Sandbox):
    """The changelog-side seam: the [Unreleased] region the collector
    splices into."""

    def test_no_unreleased_heading_is_a_refusal(self) -> None:
        self.changelog.write_text("# Changelog\n\n## [1.0.0] — 2026-11-08\n")
        self.fragment("1-one.md", "### Added\n\n- One.\n")
        with self.assertRaises(collector.CollectError) as ctx:
            collector.collect(self.changelog, self.changes)
        self.assertIn("no ## [Unreleased] heading", str(ctx.exception))


class CheckMode(Sandbox):
    def test_a_valid_pool_passes(self) -> None:
        self.fragment("README.md", "# the format\n")
        self.fragment("1-one.md", "### Added\n\n- One.\n")
        self.assertEqual(collector.check(self.changes), 0)
        self.assertEqual(self.changelog.read_text(), CHANGELOG)

    def test_an_invalid_pool_fails_and_touches_nothing(self) -> None:
        self.fragment("1-bad.md", "### Additions\n\n- No.\n")
        with self.assertRaises(collector.CollectError):
            collector.check(self.changes)
        self.assertEqual(self.changelog.read_text(), CHANGELOG)
        self.assertTrue((self.changes / "1-bad.md").exists())

    def test_main_is_the_one_reporting_site(self) -> None:
        # Both modes' refusals reach stderr through main's except clause
        # — one channel, exit 1, every finding printed.
        self.fragment("1-bad.md", "### Additions\n\n- No.\n")
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            self.assertEqual(
                collector.main(["--check", "--changes-dir", str(self.changes)]),
                1,
            )
        self.assertIn("1-bad.md", err.getvalue())
        self.assertIn("unknown section", err.getvalue())

    def test_main_requires_a_changelog_to_collect(self) -> None:
        with self.assertRaises(SystemExit) as ctx:
            collector.main(["--changes-dir", str(self.changes)])
        self.assertEqual(ctx.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
