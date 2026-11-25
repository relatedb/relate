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

The existing [Unreleased] body passes through with one change: a
fragment section whose heading the body already carries merges into it
— its entries land under that heading, idempotently (an entry the
section already carries verbatim is skipped) — because post-ADR-0627
the changelog is tooling-written only (CI refuses every CHANGELOG diff
in a pull request), a same-heading collision is a tagless re-cut's
parked prior collection, never hand-filed content. Sections the body
does not carry are appended at its end in canonical section order,
untouched body lines passing through verbatim (a first-release body may
carry headings outside the fragment vocabulary). A link-ref line or a
`## [` section heading ends the body (the first-release shape carries
no version section below it).

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
from dataclasses import dataclass
from enum import Enum, auto
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
    BULLET,
    CHANGES_DIR,
    UnreleasedRegion,
    FragmentCause,
    SECTIONS,
    SECTION_HEADING,
    Entry,
    Sections,
    append_entries,
    fragment_cause,
    fragment_number,
    parse_sections,
    render_sections,
    section_name,
    unreleased_region,
)

# The section vocabulary and canonical order, the fragment body's model
# and the grammar parser/render/merge helpers live in scripts/lib/
# changelog.py — the one spelling, shared with the rotation (which
# merges a re-cut's parked [Unreleased] into an existing version
# section through the same append_entries).

PoolRecord = tuple[Path, Sections]
Pool = list[PoolRecord]


class CollectError(Exception):
    """A refusal: the pool or the changelog does not carry what a
    collection needs."""


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


class PoolOutcome(Enum):
    """What discovery does with one pool entry."""

    KEEP = auto()  # a fragment: it pools
    SKIP = auto()  # the pool's own documentation, editor noise
    REFUSE = auto()  # a pool-shape refusal


@dataclass(frozen=True)
class Disposition:
    """One pool entry's disposition: the outcome, and the refusal message a
    REFUSE carries. The whole decision is one typed value — KEEP, SKIP or
    REFUSE(message) — instead of the two nullable layers it replaced
    (fragment_cause's `None` meaning keep, POOL_DISPOSITIONS' `None` meaning
    skip), where one `None` carried two different meanings. The message is
    present exactly when the outcome refuses; the type enforces it."""

    outcome: PoolOutcome
    message: str = ""

    def __post_init__(self) -> None:
        if (self.outcome is PoolOutcome.REFUSE) != bool(self.message):
            raise ValueError(
                "a Disposition carries a message exactly when it refuses"
            )

    @classmethod
    def refuse(cls, message: str) -> "Disposition":
        """The REFUSE disposition carrying `message`."""
        return cls(PoolOutcome.REFUSE, message)


KEEP = Disposition(PoolOutcome.KEEP)
SKIP = Disposition(PoolOutcome.SKIP)


# Every membership cause's pool disposition, one total table: the pool's
# own documentation and editor noise skip, anything else refuses with the
# message its cause earns. The table is total over the enum — a cause
# without a disposition is a loud hole at definition time, never a silent
# skip or a KeyError mid-walk — and it is the one place a cause's
# disposition is decided: pool_disposition maps a directory entry to NESTED
# and sends it here like any other cause. Every echoed entry name goes
# through log_name, so a name the pull request controls cannot break a
# refusal's line.
POOL_DISPOSITIONS: dict[
    FragmentCause, Callable[[Path, Path], Disposition]
] = {
    FragmentCause.README: lambda entry, changes_dir: SKIP,
    FragmentCause.DOTFILE: lambda entry, changes_dir: SKIP,
    FragmentCause.NESTED: lambda entry, changes_dir: Disposition.refuse(
        f"changes_collect: {log_name(entry.name)} is a directory —"
        f" fragments are flat .md files, one per pull request under"
        f" {changes_dir}"
    ),
    FragmentCause.WRONG_SUFFIX: lambda entry, changes_dir: Disposition.refuse(
        f"changes_collect: {log_name(entry.name)} is not a fragment —"
        f" one .md file per pull request under {changes_dir}"
    ),
    FragmentCause.BAD_NAME: lambda entry, changes_dir: Disposition.refuse(
        f"changes_collect: {log_name(entry.name)} is not named"
        " <PR number>-<slug>.md — the number orders the collected"
        " entries"
    ),
}


# The table's totality is enforced, not just asserted in prose: a cause
# added to the enum without a disposition fails at import, never as a
# KeyError mid-walk.
assert set(POOL_DISPOSITIONS) == set(FragmentCause)


def pool_disposition(entry: Path, changes_dir: Path) -> Disposition:
    """The one disposition of a pool entry — KEEP (a fragment), SKIP (the
    pool's own documentation, editor noise) or REFUSE naming why. The
    membership rule is fragment_cause's, the one rule, and each cause's
    disposition is POOL_DISPOSITIONS', the one table; the walk adds only
    the one fact a filesystem can see — a directory entry is NESTED, not a
    name to classify."""
    cause = (
        FragmentCause.NESTED if entry.is_dir() else fragment_cause(entry.name)
    )
    if cause is None:
        return KEEP
    return POOL_DISPOSITIONS[cause](entry, changes_dir)


def discover(changes_dir: Path) -> tuple[list[Path], list[str]]:
    """The fragment pool, in collection order, and every pool-shape
    refusal — one findings list, never one offender per run (the same
    accumulate-then-refuse discipline the content validation follows),
    with a missing directory a finding like any other: no pool to walk."""
    if not changes_dir.is_dir():
        return [], [f"changes_collect: no fragment directory at {changes_dir}"]
    fragments: list[Path] = []
    findings: list[str] = []
    for entry in sorted(changes_dir.iterdir()):
        disposition = pool_disposition(entry, changes_dir)
        if disposition.outcome is PoolOutcome.KEEP:
            fragments.append(entry)
        elif disposition.outcome is PoolOutcome.REFUSE:
            findings.append(disposition.message)
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
        except ValueError as exc:
            # The shared grammar parser refuses through ValueError; the
            # collector owns the failure surface — the channel prefix and
            # the refusal type — so the message lands as one CollectError,
            # byte-identical to the pre-shared spelling.
            findings.append(f"changes_collect: {exc}")
    return pool, findings


def splice_unreleased(
    lines: list[str], region: UnreleasedRegion, merges: Sections, contributed: Sections
) -> list[str]:
    """The changelog's new lines: the [Unreleased] body passes through
    with one change — each merged section's entries land under its
    existing heading, idempotently, completing a tagless re-cut's parked
    prior collection. A separating blank line is added when the body
    does not end with one, the contributed blocks land under it in
    canonical section order, and the remainder follows unchanged. The
    whole block layout — pass-through, merges, separator, blocks —
    lives here; collect orchestrates."""
    body = list(lines[region.heading_idx + 1 : region.end_idx])
    for name in SECTIONS:
        if name in merges:
            body = append_entries(body, name, merges[name])
    out = lines[: region.heading_idx + 1] + body
    if out[-1].strip():
        out.append("\n")
    out.extend(render_sections(contributed))
    out.extend(lines[region.end_idx:])
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

    # A fragment section the [Unreleased] body already carries merges
    # into it — the tagless re-cut's parked prior collection completing,
    # not a refusal: post-ADR-0627 the changelog is tooling-written only
    # (CI refuses every CHANGELOG diff in a pull request), so a
    # same-heading collision can only be a prior cut's own output. The
    # merge is idempotent — append_entries skips entries the section
    # already carries verbatim — so a re-run after a crashed collection
    # (entries written, fragments not deleted) adds nothing.
    merges: Sections = {}
    contributed: Sections = {}
    for fragment, sections in pool:
        for name, entries in sections.items():
            if name in existing:
                merges.setdefault(name, []).extend(entries)
            else:
                contributed.setdefault(name, []).extend(entries)

    atomic_write(
        changelog,
        "".join(splice_unreleased(lines, region, merges, contributed)),
    )

    # The deletion half of the collection: every unlink failure is named
    # in one refusal — the changelog already carries the entries, so the
    # named files must go by hand (the release sweep's rollback restores
    # both sides through git; a re-run merges idempotently — entries the
    # section already carries are skipped — never double-collecting).
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
    merged = sum(len(section) for section in merges.values())
    merged_note = f", {merged} merged into existing sections" if merged else ""
    print(
        f"changes_collect: {len(pool)} fragments ({entries} entries"
        f"{merged_note})"
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
