#!/usr/bin/env python3
"""Direct unit tests for scripts/changelog_rotate.py — the CHANGELOG
rotation policy scripts/release-bump.sh calls, including the [Unreleased]
link-ref shape discrimination (compare link = steady state, plain repository
URL = first release) and the idempotence check. The end-to-end rollback
contract stays with scripts/tests/test_release_bump.sh; here each rotation
path and each refusal is pinned against the function itself.

Run: python3 scripts/tests/test_changelog_rotate.py
"""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from script_loader import ROOT, load

changelog_rotate = load("changelog_rotate", ROOT / "scripts" / "changelog_rotate.py")

BASE = "https://github.com/relatedb/relate/"

FIRST_RELEASE = """# Changelog

## [Unreleased]

### Initial release

First release of Relate.

[Unreleased]: https://github.com/relatedb/relate
"""

STEADY_STATE = """# Changelog

## [Unreleased]

## [0.1.0] — 2026-11-08

### Initial release

First release of Relate.

[Unreleased]: https://github.com/relatedb/relate/compare/v0.1.0...HEAD

[0.1.0]: https://github.com/relatedb/relate/releases/tag/v0.1.0
"""


class RotateTests(unittest.TestCase):
    def test_first_release(self) -> None:
        out = changelog_rotate.rotate(FIRST_RELEASE, version="0.1.0", date="2026-11-08")
        self.assertIn("## [0.1.0] — 2026-11-08\n", out)
        self.assertIn(f"[Unreleased]: {BASE}compare/v0.1.0...HEAD\n", out)
        self.assertIn(f"[0.1.0]: {BASE}releases/tag/v0.1.0\n", out)
        # The new heading sits directly under the Unreleased heading.
        self.assertLess(out.index("## [Unreleased]"), out.index("## [0.1.0]"))

    def test_steady_state(self) -> None:
        out = changelog_rotate.rotate(STEADY_STATE, version="0.1.1", date="2026-11-09")
        self.assertIn("## [0.1.1] — 2026-11-09\n", out)
        self.assertIn(f"[Unreleased]: {BASE}compare/v0.1.1...HEAD\n", out)
        self.assertIn(f"[0.1.1]: {BASE}compare/v0.1.0...v0.1.1\n", out)
        # The previous version's ref survives, after the new one.
        self.assertIn(f"[0.1.0]: {BASE}releases/tag/v0.1.0\n", out)
        self.assertLess(out.index("[0.1.1]:"), out.index("[0.1.0]:"))

    def test_no_unreleased_heading_refuses(self) -> None:
        with self.assertRaises(changelog_rotate.RotateError) as ctx:
            changelog_rotate.rotate(
                "# Changelog\n\n[Unreleased]: https://github.com/relatedb/relate\n",
                version="0.1.0",
                date="2026-11-08",
            )
        self.assertIn("no ## [Unreleased] heading found", str(ctx.exception))

    def test_no_unreleased_ref_refuses(self) -> None:
        with self.assertRaises(changelog_rotate.RotateError) as ctx:
            changelog_rotate.rotate(
                "# Changelog\n\n## [Unreleased]\n",
                version="0.1.0",
                date="2026-11-08",
            )
        self.assertIn("no [Unreleased] link ref found", str(ctx.exception))

    def test_unreleased_ref_of_unknown_shape_refuses(self) -> None:
        with self.assertRaises(changelog_rotate.RotateError) as ctx:
            changelog_rotate.rotate(
                "# Changelog\n\n## [Unreleased]\n\n[Unreleased]: https://example.com/x\n",
                version="0.1.0",
                date="2026-11-08",
            )
        self.assertIn("not a plain repository URL", str(ctx.exception))

    def test_malformed_refs_refuse(self) -> None:
        # The shape patterns must not accept refs the contract refuses: a
        # query/fragment on the repo URL, an empty compare base, a
        # trailing-slash base.
        for ref in (
            "[Unreleased]: https://github.com/relatedb/relate?tab=readme",
            "[Unreleased]: https://github.com/relatedb/relate#readme",
            "[Unreleased]: /compare/v0.1.0...HEAD",
            "[Unreleased]: https://github.com/relatedb/relate//compare/v0.1.0...HEAD",
        ):
            doc = f"# Changelog\n\n## [Unreleased]\n\n{ref}\n"
            with self.subTest(ref=ref):
                with self.assertRaises(changelog_rotate.RotateError) as ctx:
                    changelog_rotate.rotate(doc, version="0.1.0", date="2026-11-08")
                self.assertIn("not a plain repository URL", str(ctx.exception))

    def test_already_rotated(self) -> None:
        self.assertTrue(changelog_rotate.already_rotated(STEADY_STATE, "0.1.0"))
        self.assertFalse(changelog_rotate.already_rotated(STEADY_STATE, "0.1.1"))
        # A link ref or prose mention is not the heading.
        self.assertFalse(changelog_rotate.already_rotated(FIRST_RELEASE, "0.1.0"))

    def test_steady_state_without_prev_ref_refuses(self) -> None:
        # A compare-link ref says steady state, but the previous version's
        # own ref is absent.
        doc = (
            "# Changelog\n\n## [Unreleased]\n\n"
            "[Unreleased]: https://github.com/relatedb/relate/compare/v0.1.0...HEAD\n"
        )
        with self.assertRaises(changelog_rotate.RotateError) as ctx:
            changelog_rotate.rotate(doc, version="0.1.1", date="2026-11-09")
        self.assertIn("no [0.1.0] link ref found", str(ctx.exception))

    def test_unreleased_ref_above_the_heading_refuses(self) -> None:
        # The splice relies on the ref sitting at or below the heading; a
        # ref above it must refuse, never silently mis-splice.
        doc = (
            "# Changelog\n\n[Unreleased]: https://github.com/relatedb/relate\n\n"
            "## [Unreleased]\n\n### Initial release\n"
        )
        with self.assertRaises(changelog_rotate.RotateError) as ctx:
            changelog_rotate.rotate(doc, version="0.1.0", date="2026-11-08")
        self.assertIn("no [Unreleased] link ref found", str(ctx.exception))

    def run_main(self, content: str, *argv: str) -> tuple[int, str]:
        """The CLI-case scaffold: write the fixture, run main, return the
        exit code and the file's resulting text."""
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "CHANGELOG.md"
            path.write_text(content)
            rc = changelog_rotate.main([str(path), *argv])
            return rc, path.read_text()

    def test_cli_roundtrip_writes_the_file(self) -> None:
        rc, text = self.run_main(FIRST_RELEASE, "--version", "0.1.0", "--date", "2026-11-08")
        self.assertEqual(rc, 0)
        self.assertIn("## [0.1.0] — 2026-11-08", text)

    def test_cli_already_rotated_skips_without_writing(self) -> None:
        rc, text = self.run_main(STEADY_STATE, "--version", "0.1.0", "--date", "2026-11-09")
        self.assertEqual(rc, 0)
        # Untouched, byte for byte.
        self.assertEqual(text, STEADY_STATE)

    def test_cli_rejects_a_non_semver_version(self) -> None:
        for bad in ("0.1", "v0.1.0", "0.1.0-rc.1"):
            with self.subTest(version=bad):
                with self.assertRaises(SystemExit) as ctx:
                    self.run_main(FIRST_RELEASE, "--version", bad, "--date", "2026-11-08")
                self.assertEqual(ctx.exception.code, 2)

    def test_cli_rejects_a_non_iso_date(self) -> None:
        for bad in ("not-a-date", "2026-13-01", "2026-11-8"):
            with self.subTest(date=bad):
                with self.assertRaises(SystemExit) as ctx:
                    self.run_main(FIRST_RELEASE, "--version", "0.1.0", "--date", bad)
                self.assertEqual(ctx.exception.code, 2)

    def test_cli_refusal_leaves_the_file_untouched(self) -> None:
        rc, text = self.run_main(
            "# Changelog\n\n## [Unreleased]\n\n[Unreleased]: https://example.com/x\n",
            "--version",
            "0.1.0",
            "--date",
            "2026-11-08",
        )
        self.assertEqual(rc, 1)
        self.assertNotIn("0.1.0", text)

    def test_cli_usage_error_without_args(self) -> None:
        with self.assertRaises(SystemExit) as ctx:
            changelog_rotate.main([])
        self.assertEqual(ctx.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
