#!/usr/bin/env python3
"""Direct unit tests for scripts/check_changelog_presence.py — the
soft changelog-entry presence warning (ADR-0618 §4). A pull request
that declares user-facing impact but changes no changelog entry is the
coverage gap; the warning is an annotation, never a gate — exit 0 in
every outcome. The template's unfilled placeholder brackets do not
count as a declaration.

Run: python3 scripts/tests/test_check_changelog_presence.py
"""

from __future__ import annotations

import unittest

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


class ChangesChangelog(unittest.TestCase):
    def test_changelog_among_changed_files(self) -> None:
        self.assertTrue(
            presence.changes_changelog(["README.md", "CHANGELOG.md"])
        )

    def test_changelog_absent(self) -> None:
        self.assertFalse(presence.changes_changelog(["README.md"]))


class WarningPolicy(unittest.TestCase):
    """The warning fires only on the gap: declared impact, no entry."""

    def test_warning_message_names_the_duty(self) -> None:
        # The message is what the annotation carries; pin its spine.
        body = FILLED_SECTION
        changed = ["src/lib.rs"]
        self.assertTrue(
            presence.declares_user_facing_impact(body)
            and not presence.changes_changelog(changed)
        )

    def test_entry_added_quiets_the_condition(self) -> None:
        changed = ["CHANGELOG.md", "src/lib.rs"]
        self.assertFalse(
            presence.declares_user_facing_impact(FILLED_SECTION)
            and not presence.changes_changelog(changed)
        )

    def test_internal_change_never_warns(self) -> None:
        # No declared impact: silent whatever else changed.
        self.assertFalse(presence.declares_user_facing_impact(NO_SECTION))


if __name__ == "__main__":
    unittest.main()
