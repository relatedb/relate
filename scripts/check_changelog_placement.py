#!/usr/bin/env python3
"""Refuse a CHANGELOG.md diff that edits anything but the [Unreleased]
section — the placement guard behind CI's changelog-placement job
(ADR-0618 §4). A merged pull request once filed its user-facing entries
under an already-released section; the release-cut rotation only collects
[Unreleased], so the entries shipped in the release but never reached its
changelog section, and the release-notes generator (ADR-0124) — which
reads exactly that section — never saw them. This check turns that silent
loss into a pull-request-time failure with a message that says where the
entry belongs.

Policy (the one home of the placement grammar; nothing else re-spells it):

  A pull request may change CHANGELOG.md only inside
  - the preamble (everything before the first `## [` heading), and
  - the [Unreleased] section (heading and body), and
  - the `[Unreleased]:` link-ref line.

  Everything else — a released version heading (`## [X.Y.Z] …`), its body,
  its `[X.Y.Z]:` link ref — is the published record and fails the check.
  Blank lines count: the refusal is about *where* the diff lands, not
  about what it says. The sanctioned escape for correcting the historical
  record is the repository's `changelog-repair` label, which the CI job
  reads before invoking this script (the script itself never grants
  escapes — it is pure text policy, same convention as
  release-notes-prepend.sh).

Classification is per side: an inserted or replaced line is classified in
the after text, a deleted line in the before text, so a hunk that only
deletes released content is caught exactly like one that edits it.

Usage: check_changelog_placement.py --before BEFORE_FILE --after AFTER_FILE

Both files must exist (the CI job materializes a missing before side as an
empty file; an empty before side with a populated after side means the
changelog is new, and every `## [` heading in it is a released heading to
refuse unless it is [Unreleased]). Exit 0 when the placement is clean,
exit 1 with one finding line per offending hunk side on stderr otherwise.
"""

from __future__ import annotations

import argparse
import difflib
import re
import sys
from pathlib import Path

# The one semver shape, kept identical to changelog_rotate.py's (the CLI
# and the section grammar never disagree about what a version looks like).
SEMVER = r"[0-9]+\.[0-9]+\.[0-9]+"

RELEASED_HEADING = re.compile(rf"^## \[(?P<version>{SEMVER})\](?P<tail>.*)$")
UNRELEASED_HEADING = "## [Unreleased]"
# A link-ref line: `[key]: destination`. The key decides the policy.
LINK_REF = re.compile(r"^\[(?P<key>[^\]]+)\]:")


def classify(lines: list[str]) -> list[str]:
    """Classify every line of one changelog text as 'unreleased' (the
    diff may touch it) or a released-section name (it may not).

    Returns a list of section names, one per line: 'preamble' or
    '[Unreleased]' for allowed lines, the released section's name (e.g.
    '[1.0.2]') for refused ones. Link-ref lines classify by their key:
    the [Unreleased] ref is allowed, a released version's ref carries
    that version's name. A released version's ref with no preceding
    heading still names its version — the ref block is the section list
    mirrored at the bottom of the file, so it polices the same record.
    """
    out: list[str] = []
    section = "preamble"
    for line in lines:
        text = line.rstrip("\n")
        heading = RELEASED_HEADING.match(text)
        if heading is not None:
            section = f"[{heading.group('version')}]"
        elif text.startswith(UNRELEASED_HEADING):
            section = "[Unreleased]"
        ref = LINK_REF.match(text)
        if ref is not None:
            key = ref.group("key")
            if key == "Unreleased":
                out.append("[Unreleased]")
            elif re.fullmatch(SEMVER, key):
                out.append(f"[{key}]")
            else:
                # A ref neither Unreleased nor a version: not part of the
                # section grammar, but still below the published record's
                # ref block line — refuse it rather than guess.
                out.append(f"[{key}]")
            continue
        out.append(section)
    return out


def check(before: str, after: str) -> list[str]:
    """The placement policy as a pure function: the CHANGELOG.md texts
    before and after a change in, the list of refusal findings out. One
    finding per refused section per hunk: a replace or insert hunk is
    reported from the added side (the text that lands), a pure delete
    from the removed side — one offending edit never stacks two
    findings. Each finding names the released section and the first
    offending line number in that side's text."""
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
        if tag in ("replace", "insert"):
            seen: dict[str, int] = {}
            for idx in range(j1, j2):
                kind = after_kinds[idx]
                if kind in ("preamble", "[Unreleased]"):
                    continue
                seen.setdefault(kind, idx + 1)
            for kind, line_no in seen.items():
                findings.append(
                    f"line {line_no} (added side): touches the released"
                    f" section {kind} — new entries go under"
                    f" '## [Unreleased]'"
                )
        if tag == "delete":
            seen_before: dict[str, int] = {}
            for idx in range(i1, i2):
                kind = before_kinds[idx]
                if kind in ("preamble", "[Unreleased]"):
                    continue
                seen_before.setdefault(kind, idx + 1)
            for kind, line_no in seen_before.items():
                findings.append(
                    f"line {line_no} (removed side): touches the released"
                    f" section {kind} — the published record is edited"
                    f" only through a labeled changelog repair"
                )
    return findings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Refuse CHANGELOG.md edits outside the [Unreleased] section"
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
            "check_changelog_placement: add new entries under"
            " '## [Unreleased]'; to correct a released section, ask a"
            " maintainer for the changelog-repair label",
            file=sys.stderr,
        )
        return 1
    print("check_changelog_placement: [Unreleased] placement clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
