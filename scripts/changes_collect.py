#!/usr/bin/env python3
"""Collect .changes/ changelog fragments into CHANGELOG.md's [Unreleased]
section — the one home of the fragment grammar (ADR-0627). Pull requests
never edit CHANGELOG.md; each files one fragment file under .changes/, and
the release cut (scripts/release-bump.sh) runs this collector before the
rotation (scripts/changelog_rotate.py, unchanged) moves the assembled
section under the version heading.

Fragment grammar (the README under .changes/ states it for humans; this
module is its one enforcement):

  - the pool is every file directly under the changes directory, except
    README.md (the directory's own documentation) and dotfiles (editor
    and OS noise); anything else visible — another suffix, a
    subdirectory — is a refusal, not an omission
  - a fragment is named `<PR number>-<slug>.md`: the number keeps
    cross-pull-request filenames from colliding and orders the collected
    entries (ascending, then by name)
  - content is one or more section blocks: a `### <Section>` heading
    (the Keep a Changelog sections plus Notes, the maintainer record),
    a body of `- ` bullets with indented wrapped continuation lines —
    the entry text verbatim, as it lands in the changelog — and nothing
    else; a section appears at most once per fragment and is never empty

The existing [Unreleased] body passes through untouched — scanned for
its section headings, never re-parsed, so a first-release body carrying
headings outside the fragment vocabulary still collects; fragment blocks
are appended at its end in canonical section order. A fragment section
whose heading already exists in that body is a refusal — the transition
window where both channels carry content ends loudly, never as two
headings in one released section. A link-ref line or a `## [` section
heading ends the body (the first-release shape carries no version
section below it).

Refusals accumulate: every invalid fragment and every body/fragment
section collision is named before one refusal, and all of them surface
through one channel — the CollectError the CLI's except clause prints to
stderr and exits 1 on (the same contract changelog_rotate.py's
RotateError carries). --check validates the pool only (CI's
fragment-validation step); the collect mode validates, splices, writes
atomically through the shared sidecar writer, and deletes the consumed
files.

Usage:
  changes_collect.py --check --changes-dir .changes
  changes_collect.py --changelog CHANGELOG.md --changes-dir .changes
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Callable

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))

from atomic_write import atomic_write
# The changelog document grammar (the [Unreleased] region both splice
# engines read), the fragment-name grammar (the membership decision and
# its causes, the PR-number derivation) and the pool's location are
# shared with the rotation, the placement guard and the presence warning
# through scripts/lib/changelog.py, the one spelling. This module is the
# one enforcement of the fragment content grammar and the pool policy.
from changelog import (
    CHANGES_DIR,
    FragmentCause,
    fragment_cause,
    fragment_number,
    unreleased_region,
)

# The section vocabulary and canonical order: the Keep a Changelog
# sections, plus Notes last (ADR-0618 §4 — maintainer record, Internal-
# rollup source only, never release-notes material).
SECTIONS = ("Added", "Changed", "Deprecated", "Removed", "Fixed", "Security", "Notes")

SECTION_HEADING = "### "
BULLET = "- "

# The fragment body's model, named once — the invariant "the innermost
# list is one entry's lines, never a section" lives in the type, not in
# prose re-derived at every boundary: an Entry is one bullet and its
# wrapped continuation lines; a fragment's parsed content is a Sections
# mapping; the validated pool is one record per fragment.
Entry = list[str]
Sections = dict[str, list[Entry]]
PoolRecord = tuple[Path, Sections]
Pool = list[PoolRecord]


class CollectError(Exception):
    """A refusal: the pool or the changelog does not carry what a
    collection needs."""


def section_name(line: str) -> str | None:
    """A section heading's name — the text after `### ` — or None when
    the line is not a section heading. The one spelling of the
    extraction: the parser and the existing-body scan both read it, so
    the name the parser accepts and the name the collision check
    compares cannot drift."""
    if not line.startswith(SECTION_HEADING):
        return None
    return line[len(SECTION_HEADING):].strip()


def parse_sections(lines: list[str], context: str) -> Sections:
    """Parse one fragment body into {section name: [entry, ...]}, an
    entry being its bullet and continuation lines, newline-stripped,
    verbatim. Everything outside the grammar is a CollectError naming
    the file: an unknown section heading, a repeated section, an empty
    section, content before the first heading, an indented line with no
    bullet to continue, no section heading at all, or a line that is
    neither a bullet, an indented continuation, nor a heading. Blank
    lines end an entry and may separate entries (rendered adjacent)."""
    sections: Sections = {}
    current: str | None = None
    entry: list[str] | None = None
    for lineno, raw in enumerate(lines, 1):
        line = raw.rstrip("\n")
        where = f"{context}:{lineno}"
        name = section_name(line)
        if name is not None:
            if name not in SECTIONS:
                raise CollectError(
                    f"changes_collect: {where}: unknown section heading"
                    f" '### {name}' — the sections are"
                    f" {', '.join(SECTIONS)}"
                )
            if name in sections:
                raise CollectError(
                    f"changes_collect: {where}: section '{name}' appears"
                    " twice — one block per section"
                )
            sections[name] = []
            current = name
            entry = None
            continue
        if not line.strip():
            entry = None
            continue
        if current is None:
            raise CollectError(
                f"changes_collect: {where}: content before the first"
                " '### ' section heading"
            )
        if line.startswith(BULLET):
            entry = [line]
            sections[current].append(entry)
        elif line[:1].isspace():
            if entry is None:
                raise CollectError(
                    f"changes_collect: {where}: an indented line with no"
                    " bullet to continue"
                )
            entry.append(line)
        else:
            raise CollectError(
                f"changes_collect: {where}: a line that is neither a"
                " '- ' bullet, an indented continuation, nor a section"
                " heading"
            )
    for name, entries in sections.items():
        if not entries:
            raise CollectError(
                f"changes_collect: {context}: section '{name}' is empty"
            )
    if not sections:
        raise CollectError(
            f"changes_collect: {context}: no '### ' section heading found"
        )
    return sections


def fragment_sort_key(path: Path) -> tuple[int, str]:
    """Ascending PR number, then name: the collection order within each
    section (ADR-0627). The number is the grammar's own derivation
    (fragment_number), never re-parsed here."""
    return (fragment_number(path.name), path.name)


def log_name(entry: str) -> str:
    """A pool entry's name as a refusal spells it: repr-quoted, so a name
    a pull request controls cannot break the line it lands in. A file
    name may carry a newline, and GitHub Actions reads a workflow command
    (`::error::…`) only where a line begins — a bare newline in a name
    would start a fresh log line the runner could act on, which the
    'changes_collect: ' prefix cannot prevent. Quoting renders the
    newline as `\\n`, mid-line, inert. Every refusal echoes the basename
    through this one helper, so one pool entry has one spelling."""
    return repr(entry)


# Every membership cause's pool disposition, one total table: None is a
# skip (the pool's own documentation, editor noise), anything else the
# refusal message the cause earns. The table is total over the enum — a
# cause without a disposition is a loud hole at definition time, never a
# silent skip or a KeyError mid-walk — and it is the one place a cause's
# disposition is decided: discover maps a directory entry to NESTED and
# sends it here like any other cause. Every echoed entry name goes
# through log_name, so a name the pull request controls cannot break a
# refusal's line.
POOL_DISPOSITIONS: dict[FragmentCause, Callable[[Path, Path], str] | None] = {
    FragmentCause.README: None,
    FragmentCause.DOTFILE: None,
    FragmentCause.NESTED: lambda entry, changes_dir: (
        f"changes_collect: {log_name(entry.name)} is a directory —"
        f" fragments are flat .md files, one per pull request under"
        f" {changes_dir}"
    ),
    FragmentCause.WRONG_SUFFIX: lambda entry, changes_dir: (
        f"changes_collect: {log_name(entry.name)} is not a fragment —"
        f" one .md file per pull request under {changes_dir}"
    ),
    FragmentCause.BAD_NAME: lambda entry, changes_dir: (
        f"changes_collect: {log_name(entry.name)} is not named"
        " <PR number>-<slug>.md — the number orders the collected"
        " entries"
    ),
}


def discover(changes_dir: Path) -> tuple[list[Path], list[str]]:
    """The fragment pool, in collection order, and every pool-shape
    refusal — one findings list, never one offender per run (the same
    accumulate-then-refuse discipline the content validation follows),
    with a missing directory a finding like any other: no pool to walk.
    The membership decision is fragment_cause's, the one rule, and each
    cause's disposition is POOL_DISPOSITIONS', the one table; the walk
    adds only the one fact a filesystem can see — a directory entry is
    NESTED, not a name to classify."""
    if not changes_dir.is_dir():
        return [], [f"changes_collect: no fragment directory at {changes_dir}"]
    fragments: list[Path] = []
    findings: list[str] = []
    for entry in sorted(changes_dir.iterdir()):
        cause = (
            FragmentCause.NESTED
            if entry.is_dir()
            else fragment_cause(entry.name)
        )
        if cause is None:
            fragments.append(entry)
            continue
        disposition = POOL_DISPOSITIONS[cause]
        if disposition is None:
            continue
        findings.append(disposition(entry, changes_dir))
    return sorted(fragments, key=fragment_sort_key), findings


def read_pool(
    changes_dir: Path,
) -> tuple[Pool, list[str]]:
    """Validate the whole pool: the parsed fragments in collection order
    as (path, sections) records — one per fragment, never two parallel
    lists to keep aligned (a fragment that fails to parse appears in no
    record, so no caller can splice its neighbors' entries under its
    name) — and every refusal, pool-shape and content alike, named
    before the caller decides, never one refusal per run."""
    fragments, findings = discover(changes_dir)
    pool: Pool = []
    for fragment in fragments:
        try:
            pool.append(
                (
                    fragment,
                    parse_sections(
                        fragment.read_text().splitlines(keepends=True),
                        # The content refusals name the fragment through
                        # log_name like every other channel: a raw
                        # newline in the name would otherwise break the
                        # `context:lineno` line into a fresh log line.
                        log_name(fragment.name),
                    ),
                )
            )
        except CollectError as exc:
            findings.append(str(exc))
    return pool, findings


def splice_unreleased(
    lines: list[str], end_idx: int, contributed: Sections
) -> list[str]:
    """The changelog's new lines: the [Unreleased] body through end_idx
    passes through verbatim (a separating blank line added when it does
    not end with one), the contributed blocks land under it in canonical
    section order, and the remainder follows unchanged. The whole block
    layout — pass-through, separator, blocks — lives here; collect
    orchestrates."""
    out = lines[:end_idx]
    if out[-1].strip():
        out.append("\n")
    out.extend(render(contributed))
    out.extend(lines[end_idx:])
    return out


def existing_headings(body: list[str]) -> set[str]:
    """The section headings already present in the [Unreleased] body —
    scanned through the one name extraction (section_name), never
    re-parsed: pre-existing changelog content passes through verbatim (a
    first-release body may carry headings outside the fragment
    vocabulary), and only a heading *name* matters, for the duplicate
    check below."""
    headings: set[str] = set()
    for raw in body:
        name = section_name(raw.rstrip("\n"))
        if name is not None:
            headings.add(name)
    return headings


def render(merged: Sections) -> list[str]:
    """The merged section blocks: canonical section order, entries
    adjacent bullets, one blank line around each block and a trailing
    blank before whatever ends the section."""
    out: list[str] = []
    for name in SECTIONS:
        entries = merged.get(name)
        if not entries:
            continue
        out.append(f"{SECTION_HEADING}{name}\n")
        out.append("\n")
        for entry in entries:
            out.extend(line + "\n" for line in entry)
        out.append("\n")
    return out


def refuse(findings: list[str]) -> None:
    """The one refusal raise — and the one guard: nothing found, no
    refusal; anything found, every finding named and joined into one
    CollectError, printed and exited on by the CLI's single except
    clause. A call site cannot forget the guard and cannot raise an
    empty refusal."""
    if findings:
        raise CollectError("\n".join(findings))


def validated_pool(changes_dir: Path) -> Pool:
    """The pool, validated: read_pool's findings routed through refuse —
    the one prelude both modes open with, so a mode cannot proceed on a
    partial pool (read_pool drops refused fragments from the records)
    and the accumulate-then-refuse contract holds at every entry."""
    pool, findings = read_pool(changes_dir)
    refuse(findings)
    return pool


def collect(changelog: Path, changes_dir: Path) -> int:
    """Validate the pool, splice its blocks under [Unreleased], write
    atomically, delete the consumed fragments."""
    pool = validated_pool(changes_dir)
    if not pool:
        print(
            f"changes_collect: no fragments under {changes_dir};"
            " [Unreleased] left untouched"
        )
        return 0

    text = changelog.read_text()
    lines = text.splitlines(keepends=True)
    region = unreleased_region(
        lines,
        refusal="changes_collect: no ## [Unreleased] heading found",
        error=CollectError,
    )
    existing = existing_headings(lines[region.heading_idx + 1 : region.end_idx])

    collisions: list[str] = []
    contributed: Sections = {}
    for fragment, sections in pool:
        for name, entries in sections.items():
            if name in existing:
                collisions.append(
                    f"changes_collect: {log_name(fragment.name)}"
                    f" contributes a '{name}' section [Unreleased] already"
                    " carries — move the entries into fragments before the"
                    " cut"
                )
            else:
                contributed.setdefault(name, []).extend(entries)
    refuse(collisions)

    atomic_write(
        changelog,
        "".join(splice_unreleased(lines, region.end_idx, contributed)),
    )

    # The deletion half of the collection: every unlink failure is named
    # in one refusal — the changelog already carries the entries, so the
    # named files must go by hand (the release sweep's rollback restores
    # both sides through git; a re-run refuses through the
    # section-collision check, never double-collecting).
    undeleted: list[Path] = []
    for fragment, _ in pool:
        try:
            fragment.unlink()
        except OSError:
            undeleted.append(fragment)
    if undeleted:
        refuse(
            [
                "changes_collect: [Unreleased] was written but consumed"
                " fragments could not be deleted — remove them by hand"
                " (their entries are already collected): "
                + ", ".join(log_name(fragment.name) for fragment in undeleted)
            ]
        )
    entries = sum(len(section) for _, sections in pool for section in sections.values())
    print(
        f"changes_collect: {len(pool)} fragments ({entries} entries)"
        f" collected into [Unreleased]; {changes_dir} cleared"
    )
    return 0


def check(changes_dir: Path) -> int:
    """Validate the pool only; touch nothing (CI's fragment step)."""
    pool = validated_pool(changes_dir)
    print(
        f"changes_collect: {len(pool)} fragments valid under"
        f" {changes_dir}"
    )
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Collect .changes/ changelog fragments into CHANGELOG.md's"
            " [Unreleased] section (ADR-0627), or validate the pool with"
            " --check"
        )
    )
    parser.add_argument(
        "--changes-dir",
        type=Path,
        default=Path(CHANGES_DIR),
        help=f"the fragment directory (default: {CHANGES_DIR})",
    )
    parser.add_argument(
        "--changelog",
        type=Path,
        help="CHANGELOG.md — required to collect, unused by --check",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="validate the fragment pool only; touch nothing",
    )
    args = parser.parse_args(argv)
    if not args.check and args.changelog is None:
        parser.error("--changelog is required to collect (or pass --check)")
    try:
        if args.check:
            return check(args.changes_dir)
        return collect(args.changelog, args.changes_dir)
    except CollectError as exc:
        print(exc, file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
