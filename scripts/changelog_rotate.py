#!/usr/bin/env python3
"""Rotate CHANGELOG.md for a release — the one implementation of the
Keep-a-Changelog rotation policy behind scripts/release-bump.sh, including
the [Unreleased] link-ref shape policy and the idempotence decision (the
heading grammar lives here only; the shell side never re-spells it):

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
exists is already rotated: the CLI prints the skip notice and exits 0
without writing.

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
import os
import re
import shutil
import sys
from dataclasses import dataclass
from datetime import date as date_type
from pathlib import Path
from typing import Callable, Sequence

UNRELEASED_REF = "[Unreleased]:"

# The one semver shape, shared by the link-ref grammar and the --version
# boundary — the CLI never emits a ref its own parser would refuse.
SEMVER = r"[0-9]+\.[0-9]+\.[0-9]+"

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


def find_line(lines: list[str], pred: Callable[[str], bool], refusal: str, start: int = 0) -> int:
    """The absolute index of the first line at or after `start` matching
    pred, or the refusal."""
    for idx in range(start, len(lines)):
        if pred(lines[idx].rstrip("\n")):
            return idx
    raise RotateError(refusal)


def parse_unreleased_ref(
    lines: list[str], *, start: int
) -> tuple[FirstRelease | SteadyState, int]:
    """The [Unreleased] link ref's shape is the mode: a compare link is
    steady state (and carries the previous version), a plain repository
    URL is the first release from this repository. Returns the mode and
    the ref's line index. `start` is required so every call states the
    lower bound explicitly — an unbounded scan is exactly the silent
    mis-splice (a ref above the heading) this must never perform."""
    ref_idx = find_line(
        lines,
        lambda l: l.startswith(UNRELEASED_REF),
        "changelog_rotate: no [Unreleased] link ref found",
        start=start,
    )
    url = lines[ref_idx].rstrip("\n")[len(UNRELEASED_REF):].strip()
    compare = COMPARE_REF.match(url)
    if compare:
        return SteadyState(base_url=compare.group("base"), prev=compare.group("prev")), ref_idx
    if REPO_URL.match(url):
        return FirstRelease(base_url=url), ref_idx
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
    heading_idx = find_line(
        lines,
        lambda l: l == version_heading("Unreleased"),
        "changelog_rotate: no ## [Unreleased] heading found",
    )
    mode, ref_idx = parse_unreleased_ref(lines, start=heading_idx + 1)
    out = lines[: heading_idx + 1]
    out += ["\n", f"{version_heading(version)} — {date}\n"]
    out += lines[heading_idx + 1 : ref_idx]
    out.append(unreleased_ref_line(mode.base_url, version))
    if isinstance(mode, FirstRelease):
        insert_at = ref_idx + 1
        insertion = ["\n", mode.new_ref_line(version)]
    else:
        insert_at = find_line(
            lines,
            lambda l: l.startswith(mode.anchor),
            f"changelog_rotate: no [{mode.prev}] link ref found",
            start=ref_idx + 1,
        )
        insertion = [mode.new_ref_line(version)]
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
        print(f"changelog_rotate: CHANGELOG already has {version_heading(args.version)}; skipping rotation")
        return 0
    try:
        rotated = rotate(text, version=args.version, date=args.date)
    except RotateError as exc:
        print(exc, file=sys.stderr)
        return 1
    # Atomic update at the caller's declared sidecar name (the bump's
    # rollback inventory already removes `<changelog>.tmp`), preserving the
    # file's mode across the replace.
    tmp = path.parent / (path.name + ".tmp")
    try:
        tmp.write_text(rotated)
        shutil.copymode(path, tmp)
        os.replace(tmp, path)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise
    return 0


if __name__ == "__main__":
    sys.exit(main())
