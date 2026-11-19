#!/usr/bin/env python3
"""Direct unit tests for scripts/check_changelog_presence.py — the
soft changelog-entry presence warning (ADR-0618 §4, ADR-0627). A pull
request that declares user-facing impact but adds no .changes/ fragment
is the coverage gap; the warning is an annotation, never a gate — exit 0
in every outcome. The template's unfilled placeholder brackets do not
count as a declaration.

Run: python3 scripts/tests/test_check_changelog_presence.py
"""

from __future__ import annotations

import contextlib
import io
import tempfile
import unittest
from pathlib import Path

from script_loader import ROOT, load

presence = load(
    "check_changelog_presence", ROOT / "scripts" / "check_changelog_presence.py"
)

TEMPLATE_SECTION = """## Summary
A change.

## User-facing impact
<Only when something a user touches changed — API, CLI, config, UI,
output.>

## Verification
- ran tests
"""

FILLED_SECTION = """## Summary
A change.

## User-facing impact
The default output mode changed; scripts parsing it must switch flags.

## Verification
- ran tests
"""

NO_SECTION = """## Summary
A change.

## Verification
- ran tests
"""


class DeclaresUserFacingImpact(unittest.TestCase):
    def test_filled_section_declares(self) -> None:
        self.assertTrue(presence.declares_user_facing_impact(FILLED_SECTION))

    def test_unfilled_placeholder_does_not_declare(self) -> None:
        self.assertFalse(presence.declares_user_facing_impact(TEMPLATE_SECTION))

    def test_absent_section_does_not_declare(self) -> None:
        self.assertFalse(presence.declares_user_facing_impact(NO_SECTION))

    def test_empty_body_does_not_declare(self) -> None:
        self.assertFalse(presence.declares_user_facing_impact(""))


class AddsFragment(unittest.TestCase):
    """Entry evidence is a .changes/ fragment (ADR-0627), named by the
    one membership rule (scripts/lib/changelog.py) — the pool's README
    and anything else under the directory that is not a fragment do not
    count, and neither does a CHANGELOG.md path: a pull request never
    edits that file."""

    def test_fragment_among_changed_files(self) -> None:
        self.assertTrue(
            presence.adds_fragment(
                ["README.md", ".changes/512-parameter-names.md"]
            )
        )

    def test_fragment_alone(self) -> None:
        self.assertTrue(presence.adds_fragment([".changes/1-first.md"]))

    def test_the_pool_readme_is_not_entry_evidence(self) -> None:
        self.assertFalse(presence.adds_fragment([".changes/README.md"]))

    def test_a_non_fragment_under_the_pool_is_not_entry_evidence(self) -> None:
        self.assertFalse(presence.adds_fragment([".changes/notes.txt"]))

    def test_a_nested_path_under_the_pool_is_not_entry_evidence(self) -> None:
        self.assertFalse(presence.adds_fragment([".changes/sub/1-x.md"]))

    def test_changelog_path_is_not_entry_evidence(self) -> None:
        self.assertFalse(
            presence.adds_fragment(["README.md", "CHANGELOG.md"])
        )

    def test_no_fragment(self) -> None:
        self.assertFalse(presence.adds_fragment(["README.md"]))


class WarningPolicy(unittest.TestCase):
    """The warning fires only on the gap: declared impact, no fragment —
    pinned through main() itself, the annotation CI actually carries
    (the ::warning:: spine and its fragment duty), and through main()'s
    silence when the duty is met."""

    def run_main(self, body: str, changed: list[str]) -> str:
        with tempfile.TemporaryDirectory() as tmp:
            pr_body = Path(tmp) / "pr-body.md"
            pr_body.write_text(body)
            changed_files = Path(tmp) / "changed-files.txt"
            changed_files.write_text("\n".join(changed) + "\n")
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(
                    presence.main(
                        ["--pr-body", str(pr_body), "--changed-files", str(changed_files)]
                    ),
                    0,
                )
        return out.getvalue()

    def test_warning_message_names_the_fragment_duty(self) -> None:
        out = self.run_main(FILLED_SECTION, ["src/lib.rs"])
        self.assertTrue(out.startswith("::warning::"), out)
        self.assertIn(".changes/", out)
        self.assertIn("ADR-0627", out)

    def test_fragment_added_quiets_the_condition(self) -> None:
        out = self.run_main(
            FILLED_SECTION, [".changes/512-parameter-names.md", "src/lib.rs"]
        )
        self.assertEqual(out, "")

    def test_the_pool_readme_does_not_quiet_the_condition(self) -> None:
        out = self.run_main(FILLED_SECTION, [".changes/README.md", "src/lib.rs"])
        self.assertTrue(out.startswith("::warning::"), out)

    def test_internal_change_never_warns(self) -> None:
        # No declared impact: silent whatever else changed.
        self.assertEqual(self.run_main(NO_SECTION, ["src/lib.rs"]), "")


if __name__ == "__main__":
    unittest.main()
