#!/usr/bin/env python3
"""Fail when the current change introduces a conflicting or unproven ADR
number, or an internal reference into ``docs/adr/``.

ADR files are ``docs/adr/NNNN-*.md`` with a four-digit, never-reused
prefix (AGENTS.md §1). A change conflicts when it adds a file whose
prefix is already present on the base ref, or when it adds two files
sharing one prefix — both ways two PRs end up claiming the same number.
One carve-out: a prefix the change itself deletes is a rename slot, so
deleting ``NNNN-old-slug.md`` beside adding ``NNNN-new-slug.md`` re-slots
one decision instead of conflicting (the claim proof still binds the
added file to the number's mint).

A change must also prove every number it adds was minted: each added
file's header block — every line before the first ``## `` section —
carries ``Claim: <sha>`` naming the commit on the ledger branch
(``adr_lib.COUNTER_BRANCH``) that claimed this file's number — the
commit ``scripts/adr_mint.py`` pushes, whose subject
``adr_lib.claim_subject`` formats. The checker does not care where in
the block the line sits; the ADR template places it after ``Status``.
The claim is checked twice: the commit must be an ancestor of the
ledger tip, and its subject must name this file's own number.

The change must also keep ``docs/adr/`` public-record clean (AGENTS.md
§1): any line it adds under ``docs/adr/`` carrying an internal
reference — a Linear ticket id (``LABS-N``, including inside
linear.app links), a CIP proposal id (``CIP-N``), or a linear.app
tracker link — fails with its file and line. The scan counts only
added lines, so ADRs that already carry internal references stay
editable (removing one is a fix, never a failure), and a same-number
re-slot is diffed against its deleted pre-image — the number rule's
own rename policy. Person names and sensitive information are review
rules there, not mechanically checkable patterns.

    python3 scripts/check_adr_numbers.py [--base REF]

``--base`` defaults to ``origin/main``. Added files are computed with
the triple-dot merge-base form (``git diff --name-only --diff-filter=A
<base>...HEAD -- docs/adr/``), so only files the change itself adds
count, not files that landed on the base after the fork point. The
script anchors itself to the repository root (``git rev-parse
--show-toplevel``), so it works from any directory inside the repo.

The ledger is resolved as ``adr_lib.COUNTER_REF``: the ``adr-numbers``
CI job fetches that branch before this script runs, and a local run
needs ``git fetch origin <COUNTER_BRANCH>`` first. When the ref is
unresolvable every added ADR fails — without the ledger nothing can
prove a number was minted.

Exit codes: 0 when no conflict, every added number is proven and no
internal reference is added; 1 on a conflict, a missing or unproven
claim, an absent ledger, or an internal reference on an added
``docs/adr/`` line (the number failures name the offending number and
point at scripts/adr_mint.py to mint a fresh one); 2 on a usage error
such as an unresolvable base ref.
"""

from __future__ import annotations

import argparse
import re
import sys
from collections import defaultdict
from collections.abc import Iterator
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from adr_lib import (
    ADR_DIR,
    COUNTER_BRANCH,
    COUNTER_REF,
    adr_entries,
    anchor_repo_root,
    checked_git,
    claim_subject,
    die,
    git,
    list_adr_paths,
    nul_split,
)

PROGRAM = "check_adr_numbers"
MINT_HINT = "mint a fresh number with scripts/adr_mint.py"
# The proof line the ADR template carries after Status, before Context.
CLAIM_LINE = re.compile(r"^Claim: ([0-9a-fA-F]{40})[ \t]*$", re.MULTILINE)
# AGENTS.md §1: ADRs are public records, so no internal references. The
# three mechanically-precise internal-reference forms in this repository:
# the Linear ticket id (LABS-N, including inside linear.app links), the
# CIP proposal id (CIP-N), and the tracker link itself. Person names and
# sensitive information are review rules there, not patterns a checker
# can pin down.
INTERNAL_REFERENCE = re.compile(r"\b(?:LABS-\d+|CIP-\d+|linear\.app)\b")
# A unified-diff hunk header: the new-file start line is the only line
# number source the added-line scan needs.
HUNK_HEADER = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@")


def base_ref(base: str) -> str:
    """argparse type for --base: an existing ref, never a dashed option.

    argparse routes an ArgumentTypeError to parser.error, so both rejections
    keep the usage-error policy (exit 2) at the single argparse path.
    """
    if base.startswith("-"):
        raise argparse.ArgumentTypeError(f"--base must not begin with '-': {base!r}")
    # An unresolvable --base is the caller's error, not an infrastructure
    # failure, so it keeps the usage-error policy (exit 2).
    result = git("rev-parse", "--verify", "--quiet", f"{base}^{{commit}}")
    if result.returncode != 0:
        raise argparse.ArgumentTypeError(f"cannot resolve base ref {base!r}")
    return base


def added_adrs(base: str) -> list[tuple[str, str]]:
    """(number, path) pairs for ADR files the change adds, sorted by path."""
    return changed_adrs(base, "A")


def deleted_adrs(base: str) -> list[tuple[str, str]]:
    """(number, path) pairs for ADR files the change deletes, sorted by path."""
    return changed_adrs(base, "D")


def diff_adr_paths(base: str, filter_: str) -> list[str]:
    """Paths of docs/adr files the change touches, per --diff-filter.

    The one changed-path query every ADR check reads (numbers, claims,
    content). ``-z`` keeps the names exact — a tab or a glob character in
    a filename survives verbatim. ``--no-renames`` is load-bearing: with
    rename detection on, git reports a similarity-detected re-slot as one
    ``R`` record that BOTH ``A`` and ``D`` filters drop, so the renamed
    file would slip past the conflict check and the claim proof entirely.
    Decomposing every rename into its A+D pair keeps the re-slot rule
    deterministic — never a function of git's similarity threshold.
    """
    result = checked_git(
        PROGRAM,
        f"git diff against {base!r} failed",
        "diff",
        "--name-only",
        "-z",
        "--no-renames",
        f"--diff-filter={filter_}",
        f"{base}...HEAD",
        "--",
        ADR_DIR,
    )
    return nul_split(result.stdout)


def changed_adrs(base: str, filter_: str) -> list[tuple[str, str]]:
    """(number, path) pairs for ADR files the change adds or deletes."""
    return sorted(adr_entries(diff_adr_paths(base, filter_)))


def base_numbers(base: str) -> set[str]:
    """The ADR numbers present on the base ref."""
    return {number for number, _ in adr_entries(list_adr_paths(PROGRAM, base))}


def counter_tip() -> str | None:
    """The fetched ledger tip as a commit, or None when the ref is absent.

    rev-parse --verify --quiet reports an unresolvable ref by exiting 1
    with empty output; any other failure (broken repo, missing git) is an
    infrastructure failure, never "no ledger".
    """
    result = git("rev-parse", "--verify", "--quiet", f"{COUNTER_REF}^{{commit}}")
    if result.returncode == 0:
        return result.stdout.strip()
    if result.returncode == 1 and not result.stdout and not result.stderr:
        return None
    die(PROGRAM, f"cannot resolve {COUNTER_REF}: {result.stderr.strip()}")


def header_block(text: str) -> str:
    """The lines before the first ``## `` section heading (the ADR header)."""
    lines: list[str] = []
    for line in text.splitlines():
        if line.startswith("## "):
            break
        lines.append(line)
    return "\n".join(lines)


def claim_sha(path: str) -> str | None:
    """The ``Claim: <40-hex>`` value from the added file, or None.

    The file is read from the working tree — the copy under review. A
    missing or malformed Claim line is None (the caller reports it); an
    unreadable file is an infrastructure failure.
    """
    try:
        text = Path(path).read_text(encoding="utf-8")
    except OSError as exc:
        die(PROGRAM, f"cannot read added ADR {path}: {exc}")
    match = CLAIM_LINE.search(header_block(text))
    return match.group(1) if match else None


def missing_claim_failure(number: str, path: str) -> str:
    """The failure for an added ADR whose header carries no claim."""
    return (
        f"{path}: no 'Claim: <40-hex>' header (after Status, before Context) "
        f"proving {number} was minted; {MINT_HINT}"
    )


def missing_ledger_failure(number: str, path: str) -> str:
    """The failure for an added ADR when the ledger branch is unresolvable."""
    return (
        f"{path}: cannot prove ADR number {number} was minted: no ledger "
        f"branch {COUNTER_REF} (fetch it with "
        f"'git fetch origin {COUNTER_BRANCH}'); {MINT_HINT}"
    )


def verify_claim(number: str, path: str, sha: str, ledger: str) -> str | None:
    """None when <sha> proves <number> was minted on the ledger, else a failure.

    Both legs are required: the claim commit must sit on the ledger branch
    (an ancestor of its tip), and its subject must name exactly this
    file's number — a claim commit for another number proves nothing
    about this one.
    """
    known = git("rev-parse", "--verify", "--quiet", f"{sha}^{{commit}}")
    if known.returncode != 0:
        return (
            f"{path}: Claim: {sha} does not name a commit in this repository; "
            f"{MINT_HINT}"
        )
    ancestor = git("merge-base", "--is-ancestor", sha, ledger)
    if ancestor.returncode == 1:
        return (
            f"{path}: Claim: {sha} is not on the ledger branch {COUNTER_REF} "
            f"(the commit that minted {number}); {MINT_HINT}"
        )
    if ancestor.returncode != 0:
        die(
            PROGRAM,
            f"cannot test {sha} against {COUNTER_REF}: {ancestor.stderr.strip()}",
        )
    subject = checked_git(
        PROGRAM,
        f"cannot read the subject of claim commit {sha}",
        "log",
        "-1",
        "--format=%s",
        sha,
    ).stdout.strip()
    expected = claim_subject(int(number))
    if subject != expected:
        return (
            f"{path}: Claim: {sha} records {subject!r}, not {expected!r} "
            f"for ADR number {number}; {MINT_HINT}"
        )
    return None


def changed_adr_paths(base: str) -> list[str]:
    """Paths of the docs/adr files whose added lines the scan reads.

    Added and modified only (``AM``, the explicit include list — not
    "everything except deletions", which would let unrelated record
    types through): a deleted file has no added lines, and renames
    cannot appear (the shared query runs ``--no-renames``). Unlike
    ``changed_adrs``, no ``adr_entries`` filtering — the public-record
    rule covers every file under docs/adr, numbered or not.
    """
    return sorted(diff_adr_paths(base, "AM"))


def adr_number(path: str) -> str | None:
    """The NNNN prefix an ADR path carries, or None when it has none.

    The same name grammar ``changed_adrs`` filters by (``adr_entries``),
    read for one path — the content rule pairs re-slots by number, so a
    non-numbered file under docs/adr simply has no pairing.
    """
    entries = adr_entries([path])
    return entries[0][0] if entries else None


def added_lines(
    base: str, path: str, pre_image: str | None
) -> Iterator[tuple[int, str]]:
    """(line number, text) for each line this change adds to path.

    ``pre_image`` is the deleted file of a same-number re-slot, or None
    for a plain addition or modification. A plain change diffs
    ``base...HEAD``; a re-slot diffs the deleted file's blob against the
    added file's, sharing the number rule's rename policy — a pure
    rename contributes no added lines, an edited one exactly its edits.

    One file per diff keeps the walk simple: a file's ``+++`` header
    always precedes its first hunk, so pre-hunk lines are headers and
    every ``+`` line after an ``@@`` header is body content — an added
    line that itself begins with ``++`` (say ``++ b/LABS-9``, a
    byte-for-byte header lookalike) can never be mistaken for one. The
    pathspec is ``:(literal)``: an ADR named ``0300-[x].md`` must not
    act as a glob character class. ``--text`` reads blobs git would
    classify as binary (a NUL byte) like any other file. Lines split on
    ``\\n`` only — ``str.splitlines`` would also tear a body line apart
    at a Unicode line separator and drop the tail unread. Every body
    line must classify as ``+``, ``-`` or the no-newline marker or the
    walk dies, fail-closed like the hunk-header leg. ``--unified=0``
    leaves the hunk header (the one header this walk does parse, for
    its line number) as the only line-number source: each ``+`` body
    line is one new-file line, counting from the hunk's ``+<start>``.
    """
    if pre_image is None:
        # Each arm supplies only its ref pair; the one diff call below
        # carries the flags, so the two arms cannot drift apart.
        revisions = (f"{base}...HEAD", "--", f":(literal){path}")
    else:
        old = checked_git(
            PROGRAM,
            f"cannot resolve the deleted {pre_image!r} on {base!r}",
            "rev-parse",
            f"{base}:{pre_image}",
        ).stdout.strip()
        new = checked_git(
            PROGRAM,
            f"cannot resolve the added {path!r} on HEAD",
            "rev-parse",
            f"HEAD:{path}",
        ).stdout.strip()
        revisions = (old, new)
    result = checked_git(
        PROGRAM,
        f"git diff for {path!r} failed",
        "diff",
        "--no-renames",
        "--unified=0",
        "--text",
        *revisions,
    )
    lines = result.stdout.split("\n")
    if lines and lines[-1] == "":
        # split's artifact after the output's final newline — not a diff
        # line. Every line below must classify, or the walk dies.
        lines.pop()
    line = 0
    in_hunk = False
    for raw in lines:
        if raw.startswith("diff --git "):
            # File boundary: the one-file contract restated as state, so
            # a pathspec that ever expands wider still parses per file.
            in_hunk = False
            continue
        if raw.startswith("@@ "):
            match = HUNK_HEADER.match(raw)
            if match is None:
                die(PROGRAM, f"cannot parse diff hunk header: {raw!r}")
            line = int(match.group(1))
            in_hunk = True
            continue
        if not in_hunk:
            # The file header block (diff --git, index, ---, +++): the
            # path already came exact from changed_adr_paths, so none of
            # it is parsed for structure.
            continue
        if raw.startswith("+"):
            yield line, raw[1:]
            line += 1
            continue
        if raw.startswith("-") or raw.startswith("\\"):
            # '-' body lines and the '\ No newline at end of file'
            # marker add no new-file line.
            continue
        die(PROGRAM, f"unrecognized diff body line: {raw!r}")


def internal_reference_failures(
    base: str, deleted: list[tuple[str, str]]
) -> list[str]:
    """Failures for internal references on lines the change adds to docs/adr/.

    Diff-aware on purpose: only added lines count, so the ADRs already
    carrying internal references stay editable (removing one is a fix,
    never a failure), while a "Later references" note that itself adds a
    reference still fails. Re-slots — ``deleted`` pairs whose number an
    added file shares — are diffed against their pre-image, the number
    rule's own rename policy rather than a second one. Paths come exact
    from ``-z`` names and line text from one ``--unified=0`` diff per
    file — no file header is parsed (the hunk header supplies only the
    line number), so no header-mimicking content line can evade the
    scan.
    """
    re_slots = {number: path for number, path in deleted}
    failures: list[str] = []
    for path in changed_adr_paths(base):
        number = adr_number(path)
        pre_image = re_slots.get(number) if number is not None else None
        for line, text in added_lines(base, path, pre_image):
            found = INTERNAL_REFERENCE.search(text)
            if found is not None:
                failures.append(
                    f"{path}:{line}: internal reference "
                    f"{found.group(0)!r} on an added line — ADRs are "
                    f"public records (AGENTS.md §1); cite ADR-NNNN or a "
                    f"public link instead"
                )
    return failures


def main() -> int:
    # Anchor before parse_args: argparse applies the base_ref type to the
    # origin/main default too, and the type's rev-parse needs the repo root.
    anchor_repo_root(PROGRAM)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--base",
        default="origin/main",
        type=base_ref,
        help="base ref the change is diffed against (default: %(default)s)",
    )
    args = parser.parse_args()

    added = added_adrs(args.base)
    # A number this change deletes is a rename slot, not a conflict:
    # deleting ``0495-old-slug.md`` and adding ``0495-new-slug.md`` re-slots
    # one decision (same number, same claim) instead of claiming a second
    # one. The claim proof below still binds the added file to the number's
    # mint, so a rename can never smuggle in an unproven number.
    deleted = deleted_adrs(args.base)
    renamed = {number for number, _ in deleted}
    open_slots = base_numbers(args.base) - renamed

    failures: list[str] = []
    clashing = [(number, path) for number, path in added if number in open_slots]
    if clashing:
        numbers = ", ".join(sorted({number for number, _ in clashing}))
        files = ", ".join(path for _, path in clashing)
        failures.append(
            f"ADR number(s) {numbers} already exist on base {args.base!r} "
            f"(added: {files}); {MINT_HINT}"
        )
    by_number: dict[str, list[str]] = defaultdict(list)
    for number, path in added:
        by_number[number].append(path)
    for number, paths in sorted(by_number.items()):
        if len(paths) > 1:
            failures.append(
                f"ADR number {number} is added more than once in this change "
                f"({', '.join(paths)}); {MINT_HINT}"
            )

    # The mint proof: every added number must be backed by its own claim
    # commit on the ledger. Probing the ledger only when the change adds an
    # ADR keeps a no-ADR change independent of the branch.
    ledger = counter_tip() if added else None
    for number, path in added:
        if ledger is None:
            failures.append(missing_ledger_failure(number, path))
            continue
        sha = claim_sha(path)
        if sha is None:
            failures.append(missing_claim_failure(number, path))
            continue
        failure = verify_claim(number, path, sha, ledger)
        if failure is not None:
            failures.append(failure)

    # AGENTS.md §1: ADRs are public records. The mechanical subset of
    # that rule — internal ticket ids and tracker links on lines this
    # change adds under docs/adr/ — fails here; person names and
    # sensitive information stay review rules.
    failures.extend(internal_reference_failures(args.base, deleted))

    if failures:
        for failure in failures:
            print(f"{PROGRAM}: {failure}", file=sys.stderr)
        return 1
    print(
        f"ok: {len(added)} ADR file(s) added "
        f"({sum(1 for number, _ in added if number in renamed)} re-slotted "
        f"rename(s)), no number conflicts, every added number minted, "
        f"no internal references on added docs/adr/ lines"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
