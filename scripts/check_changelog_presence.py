#!/usr/bin/env python3
"""The changelog-entry presence warning behind CI's Changelog placement
job (ADR-0618 §4): a pull request that describes user-facing impact but
changes no CHANGELOG.md entry is the coverage gap — release notes are
built from changelog entries, so the change would ship unreported.

Warning only, never a failure: what counts as user-facing is reviewer
judgment, and the body-section presence is a heuristic — the template's
unfilled placeholder brackets do not count as a declaration. Exit 0 in
every outcome; the warning is a PR annotation, not a gate.

Usage: check_changelog_presence.py --pr-body FILE --changed-files FILE

  --pr-body        the pull request's body text (empty file = no body)
  --changed-files  the pull request's changed paths, one per line
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

SECTION_HEADING = "## User-facing impact"


def declares_user_facing_impact(body: str) -> bool:
    """True when the body's User-facing impact section carries content:
    any non-blank line between the section heading and the next heading
    that is not the template's unfilled placeholder. The placeholder is
    angle-bracket wrapped and spans lines — a line opening a bracket
    that does not close on the same line suspends content detection
    until the closing bracket."""
    inside_section = False
    inside_placeholder = False
    for line in body.splitlines():
        stripped = line.strip()
        if stripped.startswith("## "):
            inside_section = stripped == SECTION_HEADING
            continue
        if not inside_section or not stripped:
            continue
        if inside_placeholder:
            if stripped.endswith(">"):
                inside_placeholder = False
            continue
        if stripped.startswith("<"):
            if not stripped.endswith(">"):
                inside_placeholder = True
            continue
        return True
    return False


def changes_changelog(changed_files: list[str]) -> bool:
    return any(path == "CHANGELOG.md" for path in changed_files)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Warn when a user-facing pull request has no changelog entry"
    )
    parser.add_argument("--pr-body", required=True, type=Path)
    parser.add_argument("--changed-files", required=True, type=Path)
    args = parser.parse_args(argv)

    body = args.pr_body.read_text() if args.pr_body.exists() else ""
    changed = (
        [line.strip() for line in args.changed_files.read_text().splitlines()]
        if args.changed_files.exists()
        else []
    )

    if declares_user_facing_impact(body) and not changes_changelog(changed):
        print(
            "::warning::this pull request describes user-facing impact"
            " but changes no CHANGELOG.md entry — release notes are built"
            " from changelog entries (ADR-0618); add the entry under"
            " '## [Unreleased]'"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
