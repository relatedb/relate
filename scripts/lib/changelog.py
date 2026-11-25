"""The changelog's grammar — the facts every script that reads or writes
CHANGELOG.md or the .changes/ pool consults, in one module (the code-shape
rule "don't re-split a fact"; these were comment-synchronized copies
across changelog_rotate.py, check_changelog_placement.py and
changes_collect.py, and they had already drifted). Two grammars:

- the document grammar: the one semver shape, the [Unreleased] heading,
  the released-section heading, the link-ref line, the
  locate-a-line-with-refusal scan, and the [Unreleased] region reader
  (heading index plus body end) both splice engines consult
- the fragment-name grammar (ADR-0627): a fragment file is
  `<PR number>-<slug>.md`, and the pool's README.md is never a
  fragment. fragment_cause is the grammar's one membership decision —
  is_fragment is its changed-path form (the presence warning's
  entry-evidence check), and the collector's discovery consults it for
  a pool entry, adding only each cause's refusal text

The scripts sys.path-insert this directory and import these names; their
test suites reach the same module through that import. Each name here
carries its grammar's own decision — where it answers a question, it is
the one home of that answer — and no policy beyond it: each script owns
its refusals, messages, and exits.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from enum import Enum, auto
from typing import Callable

# The one semver shape — the ref grammar, the released-heading grammar and
# every version boundary read it; no script re-spells it.
SEMVER = r"[0-9]+\.[0-9]+\.[0-9]+"

# The one [Unreleased] heading.
UNRELEASED_HEADING = "## [Unreleased]"


def is_unreleased_heading(text: str) -> bool:
    """Whether a newline-stripped line *is* the [Unreleased] heading —
    exact equality, the one matcher every reader consults (the splice
    engines and the placement guard alike). Tails belong to released
    headings only (`## [1.2.3] — 2026-11-08`); a tailed `[Unreleased]`
    line is not the heading, and prefix-tolerance lives nowhere."""
    return text == UNRELEASED_HEADING

# A released version's section heading (`## [1.2.3] — 2026-11-08`). The
# collector's body-end scan reads the same match: the [Unreleased] body
# ends where the released record begins.
RELEASED_HEADING = re.compile(rf"^## \[(?P<version>{SEMVER})\](?P<tail>.*)$")

# A link-ref line: `[key]: destination`. The key decides what a reader
# does with it.
LINK_REF = re.compile(r"^\[(?P<key>[^\]]+)\]:")

# The fragment pool's home (ADR-0627) — the location a pull request's
# changed paths name, and the collector's default --changes-dir.
CHANGES_DIR = ".changes"

# The pool's own documentation; never a fragment, never entry evidence.
README_NAME = "README.md"

# `<PR number>-<slug>.md` (ADR-0627): the number keeps cross-pull-request
# filenames from colliding and orders the collected entries. Matched
# against a fragment's stem (the suffix is checked separately, so each
# refusal can name its own cause).
FRAGMENT_NAME = re.compile(r"^[0-9]+-.+")


def find_line(
    lines: list[str],
    pred: Callable[[str], bool],
    refusal: str,
    *,
    start: int,
    error: Callable[[str], Exception],
) -> tuple[int, str]:
    """The absolute index and newline-stripped text of the first line at
    or after `start` matching pred, or the caller's refusal raised
    through `error` (each script's refusal type is its own; the scan is
    shared). `start` is keyword-only and required so every call bounds
    the scan explicitly — an unbounded scan is exactly the silent
    mis-splice (a match above the intended region) this must never
    perform."""
    for idx in range(start, len(lines)):
        text = lines[idx].rstrip("\n")
        if pred(text):
            return idx, text
    raise error(refusal)


@dataclass(frozen=True)
class UnreleasedRegion:
    """The [Unreleased] region of a changelog: its heading's line index,
    and the index where its body ends — named, so the two same-typed
    facts cannot be swapped silently."""

    heading_idx: int
    end_idx: int


def unreleased_region(
    lines: list[str], *, refusal: str, error: Callable[[str], Exception]
) -> UnreleasedRegion:
    """The [Unreleased] region of a changelog held as lines: its
    heading's index, and the index where its body ends — the next
    released-section heading or the first link ref below the heading
    (the first-release shape carries no version section), else end of
    file. The heading's absence is the caller's refusal, through
    `error`; both splice engines — the rotation and the fragment
    collector — read this one region."""
    heading_idx, _ = find_line(
        lines, is_unreleased_heading, refusal, start=0, error=error
    )
    end_idx = len(lines)
    for idx in range(heading_idx + 1, len(lines)):
        line = lines[idx].rstrip("\n")
        if RELEASED_HEADING.match(line) or LINK_REF.match(line):
            end_idx = idx
            break
    return UnreleasedRegion(heading_idx=heading_idx, end_idx=end_idx)


class FragmentCause(Enum):
    """Why a name under the fragment pool is not a fragment — the causes
    of the one membership decision (fragment_cause)."""

    README = auto()  # the pool's own documentation
    DOTFILE = auto()  # editor and OS noise
    NESTED = auto()  # not a flat pool entry
    WRONG_SUFFIX = auto()  # not a .md file
    BAD_NAME = auto()  # no <PR number>-<slug> stem


def fragment_cause(name: str) -> FragmentCause | None:
    """The one membership decision for a name under the fragment pool:
    None when the name is a fragment's, otherwise the cause. Both
    readers of the rule consult it — is_fragment for a changed path,
    the collector's discovery for a pool entry — so the rule cannot
    drift between them."""
    if name == README_NAME:
        return FragmentCause.README
    if name.startswith("."):
        return FragmentCause.DOTFILE
    if "/" in name:
        return FragmentCause.NESTED
    if not name.endswith(".md"):
        return FragmentCause.WRONG_SUFFIX
    if FRAGMENT_NAME.match(name[: -len(".md")]) is None:
        return FragmentCause.BAD_NAME
    return None


def is_fragment(path: str) -> bool:
    """True when a repository-relative `path` names a fragment file: a
    .md directly under the fragment pool with a number-prefixed stem —
    the entry channel (ADR-0627), decided by fragment_cause so nothing
    that is not a fragment can quiet the presence warning."""
    if not path.startswith(CHANGES_DIR + "/"):
        return False
    return fragment_cause(path[len(CHANGES_DIR) + 1 :]) is None


def fragment_number(name: str) -> int:
    """The PR number a validated fragment's name carries — the sort key
    of the collected entries, derived once here from the name the
    grammar validated, never re-spelled at a use site."""
    return int(name.split("-", 1)[0])


# The section vocabulary and canonical order — the Keep a Changelog
# sections, plus Notes last (ADR-0618 §4 — maintainer record, Internal-
# rollup source only, never release-notes material). Shared by the
# fragment collector and the CHANGELOG rotation: both splice engines
# read and write this one grammar, so its vocabulary lives here, once
# (the collector enforced it alone before the rotation learned to merge
# a re-cut's parked [Unreleased] into an existing version section).
SECTIONS = ("Added", "Changed", "Deprecated", "Removed", "Fixed", "Security", "Notes")

SECTION_HEADING = "### "
BULLET = "- "

# The fragment body's model, named once — the invariant "the innermost
# list is one entry's lines, never a section" lives in the type, not in
# prose re-derived at every boundary: an Entry is one bullet and its
# wrapped continuation lines; a body's parsed content is a Sections
# mapping.
Entry = list[str]
Sections = dict[str, list[Entry]]


def section_name(line: str) -> str | None:
    """A section heading's name — the text after `### ` — or None when
    the line is not a section heading. The one spelling of the
    extraction: the fragment parser, the existing-body scan and the
    rotation's merge all read it, so the name one accepts and the name
    another compares cannot drift."""
    if not line.startswith(SECTION_HEADING):
        return None
    return line[len(SECTION_HEADING):].strip()


def parse_sections(lines: list[str], context: str) -> Sections:
    """Parse one fragment-grammar body into {section name: [entry, …]},
    an entry being its bullet and continuation lines, newline-stripped,
    verbatim. Everything outside the grammar is a ValueError naming the
    position (`{context}:{lineno}`): an unknown section heading, a
    repeated section, an empty section, content before the first
    heading, an indented line with no bullet to continue, no section
    heading at all, or a line that is neither a bullet, an indented
    continuation, nor a heading. Blank lines end an entry and may
    separate entries (rendered adjacent). Callers wrap the message in
    their own refusal type and channel prefix — the grammar is shared,
    the failure surface is not."""
    sections: Sections = {}
    current: str | None = None
    entry: Entry | None = None
    for lineno, raw in enumerate(lines, 1):
        line = raw.rstrip("\n")
        where = f"{context}:{lineno}"
        name = section_name(line)
        if name is not None:
            if name not in SECTIONS:
                raise ValueError(
                    f"{where}: unknown section heading"
                    f" '### {name}' — the sections are"
                    f" {', '.join(SECTIONS)}"
                )
            if name in sections:
                raise ValueError(
                    f"{where}: section '{name}' appears"
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
            raise ValueError(
                f"{where}: content before the first"
                " '### ' section heading"
            )
        if line.startswith(BULLET):
            entry = [line]
            sections[current].append(entry)
        elif line[:1].isspace():
            if entry is None:
                raise ValueError(
                    f"{where}: an indented line with no"
                    " bullet to continue"
                )
            entry.append(line)
        else:
            raise ValueError(
                f"{where}: a line that is neither a"
                " '- ' bullet, an indented continuation, nor a section"
                " heading"
            )
    for name, entries in sections.items():
        if not entries:
            raise ValueError(f"{context}: section '{name}' is empty")
    if not sections:
        raise ValueError(f"{context}: no '### ' section heading found")
    return sections


def render_sections(merged: Sections) -> list[str]:
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


def _section_entries(section_lines: list[str]) -> list[Entry]:
    """One section's rendered lines grouped into entries — a bullet and
    its continuations — for the idempotent merge's duplicate check."""
    entries: list[Entry] = []
    entry: Entry | None = None
    for raw in section_lines:
        line = raw.rstrip("\n")
        if not line.strip():
            entry = None
            continue
        if line.startswith(BULLET):
            entry = [line]
            entries.append(entry)
        elif entry is not None:
            entry.append(line)
    return entries


def append_entries(body: list[str], name: str, entries: list[Entry]) -> list[str]:
    """Merge `entries` into `name`'s section of a rendered body: they
    land after the section's last bullet, before the section's trailing
    blank line, in the order given. An entry the section already carries
    verbatim is skipped — the merge is idempotent, so a re-run after a
    crashed splice (entries written, the source not consumed) adds
    nothing. A body without the section heading is a caller bug, raised
    loudly rather than appended at a guessed position."""
    heading = f"{SECTION_HEADING}{name}\n"
    try:
        head = body.index(heading)
    except ValueError:
        raise ValueError(f"append_entries: no '### {name}' section in the body") from None
    end = len(body)
    for idx in range(head + 1, len(body)):
        if section_name(body[idx].rstrip("\n")) is not None:
            end = idx
            break
    insert = end
    while insert > head + 1 and not body[insert - 1].strip():
        insert -= 1
    already = _section_entries(body[head + 1 : insert])
    out = body[:insert]
    for entry in entries:
        if entry in already:
            continue
        out.extend(line + "\n" for line in entry)
    return out + body[insert:]
