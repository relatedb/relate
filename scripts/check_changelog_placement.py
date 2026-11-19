#!/usr/bin/env python3
"""Refuse any CHANGELOG.md diff in a pull request — the placement guard
behind CI's changelog-placement job. Changelog entries arrive as .changes/
fragment files (ADR-0627), one per pull request; the release cut collects
them into the version section and clears the directory. CHANGELOG.md is
release-owned: the only writers are the release machinery's own commits on
main (the bump, the stamp, the advance), which never travel through a
pull request, and the changelog-repair escape below.

Before ADR-0627 the guard allowed pull requests to edit the [Unreleased]
section; every entry-carrying pull request then conflicted with every
other at rebase time, and re-applying an entry around those conflicts is
how entries landed under released sections (ADR-0618 §4's original
failure). Refusing the whole file closes both: the conflict source is
gone, and the published record stays untouched.

Policy (the one home of the placement grammar; nothing else re-spells it):

  A pull request may not change CHANGELOG.md at all — not the released
  sections, not [Unreleased], not the preamble, not the link refs.

  The sanctioned escape for correcting the published record is the
  repository's changelog-repair label, which the CI job reads before
  invoking this script (the script itself never grants escapes — it is
  pure text policy, same convention as release-notes-prepend.sh).

Classification is per side: an inserted or replaced line is classified in
the after text, a deleted line in the before text, so a hunk that only
deletes released content is caught exactly like one that edits it. Each
finding names the section it touches, so the refusal says what was being
edited, not merely that something was.

Usage: check_changelog_placement.py --before BEFORE_FILE --after AFTER_FILE

Both files must exist (the CI job materializes a missing before side as an
empty file; an empty before side with a populated after side means the
changelog is new, and every hunk in it is a refusal). Exit 0 when the pull
request changes no line of CHANGELOG.md, exit 1 with one finding line per
offending hunk side on stderr otherwise.
"""

from __future__ import annotations

import argparse
import difflib
import re
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))

# The changelog document grammar — the [Unreleased] heading, the
# released-section heading, the link-ref line, the semver — is shared
# with the rotation and the fragment collector through
# scripts/lib/changelog.py, the one spelling.
from changelog import LINK_REF, RELEASED_HEADING, SEMVER, is_unreleased_heading


@dataclass(frozen=True)
class Preamble:
    """Everything before the first section heading."""


@dataclass(frozen=True)
class Unreleased:
    """The [Unreleased] section."""


@dataclass(frozen=True)
class Released:
    """A released version's section; `version` is its display name."""

    version: str


@dataclass(frozen=True)
class OtherRef:
    """A link ref keyed outside the section grammar — neither
    [Unreleased] nor a version; `key` is its display name."""

    key: str


Line = Preamble | Unreleased | Released | OtherRef


def classify(lines: list[str]) -> list[Line]:
    """Classify every line of one changelog text by the section it
    belongs to, as a typed classification: Preamble, Unreleased, or
    Released(version); link-ref lines classify by their key — the
    [Unreleased] ref is Unreleased, a released version's ref is
    Released(version), any other key is OtherRef(key). The types are the
    discriminator; the display names are attributes, so the refusal
    names exactly what was touched."""
    out: list[Line] = []
    section: Line = Preamble()
    for line in lines:
        text = line.rstrip("\n")
        heading = RELEASED_HEADING.match(text)
        if heading is not None:
            section = Released(version=heading.group("version"))
            out.append(section)
            continue
        if is_unreleased_heading(text):
            section = Unreleased()
            out.append(Unreleased())
            continue
        ref = LINK_REF.match(text)
        if ref is not None:
            key = ref.group("key")
            if key == "Unreleased":
                out.append(Unreleased())
            elif re.fullmatch(SEMVER, key):
                out.append(Released(version=key))
            else:
                out.append(OtherRef(key=key))
            continue
        out.append(section)
    return out


def finding_for(kind: Line, side: str, line_no: int) -> str:
    """The one message per touched classification — an explicit arm per
    variant, no catch-all: where the edit landed and what to do
    instead."""
    where = f"line {line_no} ({side})"
    if isinstance(kind, Preamble):
        return (
            f"{where}: touches the changelog preamble — the whole file is"
            " release-owned; a wording change needs the changelog-repair"
            " label"
        )
    if isinstance(kind, Unreleased):
        return (
            f"{where}: touches the [Unreleased] section — it is assembled"
            " from .changes/ fragments at the release cut; add"
            " .changes/<PR number>-<slug>.md instead (see"
            " .changes/README.md)"
        )
    if isinstance(kind, Released):
        return (
            f"{where}: touches the released section [{kind.version}] —"
            " the published record is edited only through a labeled"
            " changelog repair"
        )
    return (
        f"{where}: touches the [{kind.key}] ref — a link ref outside the"
        " section grammar, still release-owned; editing it needs the"
        " changelog-repair label"
    )


def check(before: str, after: str) -> list[str]:
    """The placement policy as a pure function: the CHANGELOG.md texts
    before and after a change in, the list of refusal findings out. One
    finding per touched section per hunk: a replace or insert hunk is
    reported from the added side (the text that lands), a pure delete
    from the removed side — one offending edit never stacks two
    findings. Each finding names the section and the first offending
    line number in that side's text."""
    before_lines = before.splitlines(keepends=True)
    after_lines = after.splitlines(keepends=True)
    before_kinds = classify(before_lines)
    after_kinds = classify(after_lines)

    findings: list[str] = []
    matcher = difflib.SequenceMatcher(
        a=before_lines, b=after_lines, autojunk=False
    )
    for tag, i1, i2, j1, j2 in matcher.get_opcodes():
        if tag == "equal":
            continue
        # One scan-and-emit body: the side the hunk lands on selects the
        # kinds, the index range, and the label; a replace or insert is
        # reported from the added side (the text that lands), a pure
        # delete from the removed side.
        if tag == "delete":
            kinds, lo, hi, side = before_kinds, i1, i2, "removed side"
        else:
            kinds, lo, hi, side = after_kinds, j1, j2, "added side"
        seen: dict[Line, int] = {}
        for idx in range(lo, hi):
            seen.setdefault(kinds[idx], idx + 1)
        for kind, line_no in seen.items():
            findings.append(finding_for(kind, side, line_no))
    return findings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Refuse any CHANGELOG.md edit in a pull request"
    )
    parser.add_argument(
        "--before", required=True, type=Path, help="CHANGELOG.md before the change"
    )
    parser.add_argument(
        "--after", required=True, type=Path, help="CHANGELOG.md after the change"
    )
    args = parser.parse_args(argv)

    before = args.before.read_text() if args.before.exists() else ""
    after = args.after.read_text() if args.after.exists() else ""
    findings = check(before, after)
    if findings:
        for finding in findings:
            print(f"check_changelog_placement: {finding}", file=sys.stderr)
        print(
            "check_changelog_placement: a pull request never changes"
            " CHANGELOG.md — file the entry as a .changes/ fragment; to"
            " correct the published record, ask a maintainer for the"
            " changelog-repair label",
            file=sys.stderr,
        )
        return 1
    print("check_changelog_placement: CHANGELOG.md untouched")
    return 0


if __name__ == "__main__":
    sys.exit(main())
