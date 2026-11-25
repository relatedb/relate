#!/usr/bin/env python3
"""Rotate CHANGELOG.md for a release — the one implementation of the
Keep-a-Changelog rotation policy behind scripts/release-bump.sh, including
the [Unreleased] link-ref shape policy and the idempotence decision (the
document grammar is shared through scripts/lib/changelog.py; the shell
side never re-spells it):

  - `<base>/compare/v<PREV>...HEAD` — steady state: the new version's ref
    compares against v<PREV> (`<base>/compare/v<PREV>...v<NEXT>`, inserted
    at the `[<PREV>]:` ref, which must exist).
  - a plain repository URL (`https://github.com/owner/repo`) — the first
    release from this repository: no prior tag exists, so the new version's
    ref points at the release page (`<url>/releases/tag/v<NEXT>`, appended
    after the `[Unreleased]` ref), never at a compare.

Either way the rotation inserts `## [<next>] — <date>` directly under the
`## [Unreleased]` heading and rewrites the `[Unreleased]:` ref to
`<base>/compare/v<next>...HEAD`. `base` never carries a trailing slash;
the line builders add it. A changelog whose `## [<next>]` heading already
exists is a re-run of the same cut: with `[Unreleased]` empty the CLI
prints the skip notice and exits 0 without writing; with it populated —
a tagless re-cut (the release failed before tagging, the next dispatch
re-derives the same version) collected the next pool under it — the
entries merge into the existing version section, idempotently (an entry
the section already carries verbatim is skipped), and `[Unreleased]`
empties. `[Unreleased]` content the fragment grammar cannot parse is a
refusal, never a guessed merge.

Refusals come through two channels: document-shape refusals (no
`## [Unreleased]` heading, no `[Unreleased]:` link ref at or below the
heading, a link ref of any other shape, a steady-state rotation whose
`[<prev>]:` ref is absent) raise RotateError — the CLI prints them to
stderr and exits 1 with the file untouched (the document is built in
memory first and written atomically). argv-shape refusals (a `--version`
that is not X.Y.Z, a `--date` that is not YYYY-MM-DD) are refused at the
argparse boundary — a usage error, exit 2.

Usage:
  changelog_rotate.py CHANGELOG.md --version X.Y.Z --date YYYY-MM-DD
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from datetime import date as date_type
from pathlib import Path
from typing import Sequence

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))

from atomic_write import atomic_write
# The changelog document grammar — semver, the [Unreleased] heading, the
# locate-a-line-with-refusal scan — and, since the rotation learned to
# merge a tagless re-cut's parked [Unreleased] into the existing version
# section, the fragment section grammar the collector renders it with:
# shared through scripts/lib/changelog.py, the one spelling (the
# refusal type stays this module's own).
from changelog import (
    SECTIONS,
    SECTION_HEADING,
    SEMVER,
    Entry,
    Sections,
    append_entries,
    parse_sections,
    render_sections,
    section_name,
    find_line,
    unreleased_region,
)

UNRELEASED_REF = "[Unreleased]:"

# The base is non-empty and never ends in a slash (the line builders add
# it); the repo URL's two segments are GitHub's owner/repo alphabet, so a
# query or fragment is a refusal, not a match.
COMPARE_REF = re.compile(rf"^(?P<base>\S+?[^/\s])/compare/v(?P<prev>{SEMVER})\.\.\.HEAD$")
REPO_URL = re.compile(r"^https://github\.com/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$")


class RotateError(Exception):
    """A refusal: the changelog does not carry what a rotation needs."""


def semver_arg(value: str) -> str:
    """argparse type: --version is exactly X.Y.Z."""
    if not re.fullmatch(SEMVER, value):
        raise argparse.ArgumentTypeError(f"version '{value}' is not X.Y.Z")
    return value


def date_arg(value: str) -> str:
    """argparse type: --date is exactly YYYY-MM-DD (a real date)."""
    try:
        return date_type.fromisoformat(value).isoformat()
    except ValueError:
        raise argparse.ArgumentTypeError(f"date '{value}' is not YYYY-MM-DD") from None


def version_heading(version: str) -> str:
    """The one spelling of a release section heading."""
    return f"## [{version}]"


def already_rotated(text: str, version: str) -> bool:
    """The idempotence decision: the version's heading already exists. A
    prefix match — the heading line carries the ` — <date>` suffix."""
    heading = version_heading(version)
    return any(line.rstrip("\n").startswith(heading) for line in text.splitlines())


def _is_link_ref(line: str) -> bool:
    """A link-reference line (`[<name>]: <url>`) — the ref block that
    ends the last version section."""
    return line.startswith("[") and "]: " in line


def merge_recut(text: str, version: str) -> str | None:
    """The tagless re-cut's completion: the version's section already
    exists (a prior cut rotated, the release never tagged) and
    [Unreleased] carries the next collection — merge those entries into
    the existing section, idempotently, and empty [Unreleased]. None
    when there is nothing to merge (the pure re-run skip); a RotateError
    when [Unreleased] carries content the fragment grammar cannot parse
    — the merge never guesses at non-grammar body lines."""
    lines = text.splitlines(keepends=True)
    region = unreleased_region(
        lines,
        refusal="changelog_rotate: no ## [Unreleased] heading found",
        error=RotateError,
    )
    body = lines[region.heading_idx + 1 : region.end_idx]
    if not any(line.strip() for line in body):
        return None
    try:
        merges = parse_sections(body, "the [Unreleased] body")
    except ValueError as exc:
        raise RotateError(
            "changelog_rotate: [Unreleased] carries content the fragment"
            f" grammar cannot parse, so it cannot merge into {version_heading(version)}: {exc}"
        ) from None

    ver_idx, _ = find_line(
        lines,
        lambda l: l.rstrip("\n").startswith(version_heading(version)),
        f"changelog_rotate: no {version_heading(version)} heading found",
        start=region.end_idx,
        error=RotateError,
    )
    # The version section runs to the next version heading or the ref
    # block, whichever ends it first.
    section_end = len(lines)
    for idx in range(ver_idx + 1, len(lines)):
        if lines[idx].startswith("## [") or _is_link_ref(lines[idx].rstrip("\n")):
            section_end = idx
            break
    section = list(lines[ver_idx + 1 : section_end])

    # Entries under headings the section already carries, idempotently;
    # whole blocks — in canonical position — for the ones it does not.
    blocks: Sections = {}
    for name in SECTIONS:
        entries = merges.get(name)
        if not entries:
            continue
        if f"{SECTION_HEADING}{name}\n" in section:
            section = append_entries(section, name, entries)
        else:
            blocks[name] = entries
    for name in SECTIONS:
        entries = blocks.get(name)
        if not entries:
            continue
        insert_at = _block_insert_at(section, name)
        section[insert_at:insert_at] = render_sections({name: entries})[:-1]

    out = lines[: region.heading_idx + 1]
    out.append("\n")
    out += lines[region.end_idx : ver_idx + 1]
    out += section
    out += lines[section_end:]
    return "".join(out)


def _block_insert_at(section: list[str], name: str) -> int:
    """Where a new section block lands in the version section: canonically
    — before the first section whose canonical order follows it, after
    the last that precedes it, or at the section's start when none
    precedes it."""
    following = SECTIONS[SECTIONS.index(name) + 1 :]
    for idx, raw in enumerate(section):
        other = section_name(raw.rstrip("\n"))
        if other is not None and other in following:
            return idx
    for idx in range(len(section) - 1, -1, -1):
        if section_name(section[idx].rstrip("\n")) is not None:
            return idx + 1
    return 0


def unreleased_ref_line(base_url: str, version: str) -> str:
    """The rewritten [Unreleased] ref — mode-independent, defined once."""
    return f"[Unreleased]: {base_url}/compare/v{version}...HEAD\n"


@dataclass(frozen=True)
class FirstRelease:
    """No prior tag: the new version's ref points at the release page and
    is emitted right after the rewritten [Unreleased] ref."""

    base_url: str

    def new_ref_line(self, version: str) -> str:
        return f"[{version}]: {self.base_url}/releases/tag/v{version}\n"

    def insertion(
        self, lines: list[str], ref_idx: int, version: str
    ) -> tuple[int, list[str]]:
        """Where the new ref goes and what it is: the constant position
        right after the [Unreleased] ref, separated by a blank line. This
        arm scans nothing (`lines` is unused — the signature is the mode
        protocol's) and cannot refuse: a first release has no earlier ref
        to anchor at."""
        return ref_idx + 1, ["\n", self.new_ref_line(version)]


@dataclass(frozen=True)
class SteadyState:
    """A prior tag exists: the new version's ref compares against it and is
    emitted at the previous version's own ref (the anchor)."""

    base_url: str
    prev: str

    @property
    def anchor(self) -> str:
        return f"[{self.prev}]:"

    def new_ref_line(self, version: str) -> str:
        return f"[{version}]: {self.base_url}/compare/v{self.prev}...v{version}\n"

    def insertion(
        self, lines: list[str], ref_idx: int, version: str
    ) -> tuple[int, list[str]]:
        """Where the new ref goes and what it is: at the previous version's
        own ref (the anchor), which must exist below the [Unreleased] ref."""
        insert_at, _ = find_line(
            lines,
            lambda l: l.startswith(self.anchor),
            f"changelog_rotate: no [{self.prev}] link ref found",
            start=ref_idx + 1,
            error=RotateError,
        )
        return insert_at, [self.new_ref_line(version)]


@dataclass(frozen=True)
class UnreleasedRef:
    """The [Unreleased] link ref: its release mode and its line index."""

    mode: FirstRelease | SteadyState
    line_idx: int


def parse_unreleased_ref(lines: list[str], *, start: int) -> UnreleasedRef:
    """The [Unreleased] link ref's shape is the mode: a compare link is
    steady state (and carries the previous version), a plain repository
    URL is the first release from this repository. Returns the mode and
    the ref's line index. `start` is required so every call states the
    lower bound explicitly — an unbounded scan is exactly the silent
    mis-splice (a ref above the heading) this must never perform."""
    ref_idx, ref_line = find_line(
        lines,
        lambda l: l.startswith(UNRELEASED_REF),
        "changelog_rotate: no [Unreleased] link ref found",
        start=start,
        error=RotateError,
    )
    url = ref_line[len(UNRELEASED_REF):].strip()
    compare = COMPARE_REF.match(url)
    if compare:
        return UnreleasedRef(
            SteadyState(base_url=compare.group("base"), prev=compare.group("prev")), ref_idx
        )
    if REPO_URL.match(url):
        return UnreleasedRef(FirstRelease(base_url=url), ref_idx)
    raise RotateError(
        "changelog_rotate: cannot derive previous version from the"
        " [Unreleased] link ref, and it is not a plain repository"
        " URL for a first release either"
    )


def rotate(text: str, *, version: str, date: str) -> str:
    """Locate-then-splice: the heading index, the [Unreleased] ref index
    (at or below the heading — the ordering invariant is enforced by the
    search, not a guard), and (steady state) the anchor index are looked
    up once — each missing position is its refusal. Each mode computes
    only where and what to insert; the splice happens once."""
    lines = text.splitlines(keepends=True)
    region = unreleased_region(
        lines,
        refusal="changelog_rotate: no ## [Unreleased] heading found",
        error=RotateError,
    )
    ref = parse_unreleased_ref(lines, start=region.heading_idx + 1)
    mode, ref_idx = ref.mode, ref.line_idx
    out = lines[: region.heading_idx + 1]
    out += ["\n", f"{version_heading(version)} — {date}\n"]
    out += lines[region.heading_idx + 1 : ref_idx]
    out.append(unreleased_ref_line(mode.base_url, version))
    insert_at, insertion = mode.insertion(lines, ref_idx, version)
    out += lines[ref_idx + 1 : insert_at]
    out += insertion
    out += lines[insert_at:]
    return "".join(out)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Rotate CHANGELOG.md for a release")
    parser.add_argument("changelog", type=Path)
    parser.add_argument("--version", required=True, type=semver_arg)
    parser.add_argument("--date", required=True, type=date_arg)
    args = parser.parse_args(argv)

    path: Path = args.changelog
    text = path.read_text()
    if already_rotated(text, args.version):
        # A re-run of the same cut: with [Unreleased] empty this is the
        # pure idempotence skip; with it populated (a tagless re-cut
        # collected the next pool under it) the entries merge into the
        # existing version section — the release sweep's rollback
        # restores both sides through git, and the merge is idempotent,
        # so a crashed rotation's re-run adds nothing.
        try:
            merged = merge_recut(text, args.version)
        except RotateError as exc:
            print(exc, file=sys.stderr)
            return 1
        if merged is None:
            print(f"changelog_rotate: CHANGELOG already has {version_heading(args.version)}; skipping rotation")
            return 0
        atomic_write(path, merged)
        print(
            f"changelog_rotate: CHANGELOG already has {version_heading(args.version)};"
            " merged the re-cut's [Unreleased] entries into it"
        )
        return 0
    try:
        rotated = rotate(text, version=args.version, date=args.date)
    except RotateError as exc:
        print(exc, file=sys.stderr)
        return 1
    # Atomic update through the shared sidecar writer (scripts/lib/
    # atomic_write.py), so an interrupt never truncates the CHANGELOG.
    atomic_write(path, rotated)
    return 0


if __name__ == "__main__":
    sys.exit(main())
