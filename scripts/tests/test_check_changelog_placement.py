#!/usr/bin/env python3
"""Direct unit tests for scripts/check_changelog_placement.py — the
CHANGELOG.md placement guard CI's changelog-placement job runs
(ADR-0618 §4). Each pin covers one hunk side: where the diff lands decides
the verdict, blank lines included, and every finding names the released
section it touched. The end-to-end job wiring (label escape, base-ref
diff) stays with the workflow; here the pure policy is pinned against the
function itself.

Run: python3 scripts/tests/test_check_changelog_placement.py
"""

from __future__ import annotations

import unittest

from script_loader import ROOT, load

guard = load(
    "check_changelog_placement", ROOT / "scripts" / "check_changelog_placement.py"
)

BEFORE = """# Changelog

All notable changes to Relate are documented in this file.

## [Unreleased]

## [1.0.2] — 2026-11-14

### Added

- Swift: `Database(url:readOnly:)` and typed namespaces.

### Changed

- `fs-to-graph` commits each rows record as its own transaction.

## [1.0.1] — 2026-11-12

### Added

- Bolt: the server reports its version.

[Unreleased]: https://github.com/relatedb/relate/compare/v1.0.2...HEAD
[1.0.2]: https://github.com/relatedb/relate/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/relatedb/relate/compare/v1.0.0...v1.0.1
"""


class PlacementClean(unittest.TestCase):
    """Diffs the guard passes: the [Unreleased] section, the preamble,
    and the [Unreleased] link ref."""

    def test_no_change_is_clean(self):
        self.assertEqual(guard.check(BEFORE, BEFORE), [])

    def test_entry_added_under_unreleased(self):
        after = BEFORE.replace(
            "## [Unreleased]\n",
            "## [Unreleased]\n\n### Added\n\n- CLI: a new flag.\n",
            1,
        )
        self.assertEqual(guard.check(BEFORE, after), [])

    def test_subheading_added_under_unreleased(self):
        after = BEFORE.replace(
            "## [Unreleased]\n", "## [Unreleased]\n\n### Changed\n", 1
        )
        self.assertEqual(guard.check(BEFORE, after), [])

    def test_preamble_edit_is_clean(self):
        after = BEFORE.replace(
            "All notable changes to Relate are documented in this file.",
            "All notable changes to Relate are documented here.",
            1,
        )
        self.assertEqual(guard.check(BEFORE, after), [])

    def test_unreleased_ref_edit_is_clean(self):
        after = BEFORE.replace(
            "[Unreleased]: https://github.com/relatedb/relate/compare/v1.0.2...HEAD",
            "[Unreleased]: https://github.com/relatedb/relate/compare/v1.0.3...HEAD",
            1,
        )
        self.assertEqual(guard.check(BEFORE, after), [])

    def test_empty_before_with_unreleased_only_is_clean(self):
        after = "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- A fresh start.\n"
        self.assertEqual(guard.check("", after), [])


class PlacementRefused(unittest.TestCase):
    """Diffs the guard refuses: released headings, released bodies,
    released link refs — on the added side and the removed side alike."""

    def test_entry_added_under_released_section(self):
        after = BEFORE.replace(
            "- Swift: `Database(url:readOnly:)` and typed namespaces.\n",
            "- Swift: `Database(url:readOnly:)` and typed namespaces.\n"
            "- Swift: bridges for dates, durations, vectors and points.\n",
            1,
        )
        findings = guard.check(BEFORE, after)
        self.assertEqual(len(findings), 1)
        self.assertIn("[1.0.2]", findings[0])
        self.assertIn("Unreleased", findings[0])
        self.assertIn("added side", findings[0])

    def test_entry_removed_from_released_section(self):
        after = BEFORE.replace(
            "- `fs-to-graph` commits each rows record as its own transaction.\n",
            "",
            1,
        )
        findings = guard.check(BEFORE, after)
        self.assertEqual(len(findings), 1)
        self.assertIn("[1.0.2]", findings[0])
        self.assertIn("removed side", findings[0])
        self.assertIn("changelog repair", findings[0])

    def test_released_heading_edit_is_refused(self):
        after = BEFORE.replace(
            "## [1.0.2] — 2026-11-14", "## [1.0.2] — 2026-11-15", 1
        )
        findings = guard.check(BEFORE, after)
        self.assertEqual(len(findings), 1)
        self.assertIn("[1.0.2]", findings[0])

    def test_new_released_section_is_refused(self):
        # The release-cut rotation shape: a version heading and its body
        # inserted below [Unreleased]. The rotation runs on main through
        # release-bot, never through a pull request — here it must refuse.
        after = BEFORE.replace(
            "## [Unreleased]\n",
            "## [Unreleased]\n\n## [1.1.0] — 2026-11-17\n\n### Changed\n\n"
            "- Shortest-path endpoints filter by their WHERE.\n",
            1,
        )
        findings = guard.check(BEFORE, after)
        self.assertTrue(
            any("[1.1.0]" in f for f in findings), findings
        )

    def test_released_ref_edit_is_refused(self):
        after = BEFORE.replace(
            "[1.0.2]: https://github.com/relatedb/relate/compare/v1.0.1...v1.0.2",
            "[1.0.2]: https://github.com/relatedb/relate/compare/v1.0.0...v1.0.2",
            1,
        )
        findings = guard.check(BEFORE, after)
        self.assertEqual(len(findings), 1)
        self.assertIn("[1.0.2]", findings[0])

    def test_new_released_ref_is_refused(self):
        after = BEFORE.replace(
            "[Unreleased]: https://github.com/relatedb/relate/compare/v1.0.2...HEAD",
            "[1.1.0]: https://github.com/relatedb/relate/compare/v1.0.2...v1.1.0\n"
            "[Unreleased]: https://github.com/relatedb/relate/compare/v1.1.0...HEAD",
            1,
        )
        findings = guard.check(BEFORE, after)
        self.assertEqual(
            [f for f in findings if "[1.1.0]" in f],
            [
                "line 23 (added side): touches the released section [1.1.0]"
                " — new entries go under '## [Unreleased]'"
            ],
        )

    def test_blank_line_inside_released_section_is_refused(self):
        # Strict by design: the refusal is about where the hunk lands.
        after = BEFORE.replace(
            "- Swift: `Database(url:readOnly:)` and typed namespaces.\n",
            "- Swift: `Database(url:readOnly:)` and typed namespaces.\n\n",
            1,
        )
        findings = guard.check(BEFORE, after)
        self.assertEqual(len(findings), 1)
        self.assertIn("[1.0.2]", findings[0])

    def test_empty_before_with_released_section_is_refused(self):
        after = "# Changelog\n\n## [Unreleased]\n\n## [1.0.0] — 2026-11-08\n\n- First release.\n"
        findings = guard.check("", after)
        self.assertTrue(any("[1.0.0]" in f for f in findings), findings)


if __name__ == "__main__":
    unittest.main()
