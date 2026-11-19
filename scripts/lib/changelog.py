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
