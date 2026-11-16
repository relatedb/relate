#!/usr/bin/env python3
"""Fail when a deny.toml ignore entry is past its expiry comment.

The [advisories] policy in deny.toml (its own header states it) is that
every ignore entry carries a reason and an expiry: an entry past its
expiry is a re-triage prompt, not a standing accept. cargo-deny 0.20.x
cannot enforce that — its ignore entries accept only ``id`` and
``reason``, so the expiry rides as a ``# expiry: YYYY-MM-DD`` comment
on the entry's ``id`` line — and without a mechanical check an expired
entry stays green forever, the silent decay into standing accepts the
policy exists to prevent.

Entry discovery is structural: tomllib's [advisories].ignore list, the
TOML parser's own view — never a regex over the file — so no quote
style and no entry form hides an advisory from the policy (a
single-quoted id and a ``[[advisories.ignore]]`` table entry are
entries like any other). The expiry is read only from the comment on
the entry's id line, the region after every quoted string: expiry-like
text inside a reason string is never the entry's expiry. A valid-TOML
shape the check cannot see — an entry it cannot tie to an id line —
fails loudly rather than riding unenforced.

The check fails on any expiry before today (UTC): an entry expiring
today is current, one from yesterday is a re-triage prompt. The weekly
cargo-deny workflow runs it after the scan, and ``make
test-deny-expiry`` (a test-scripts prerequisite) runs the suite
including the live deny.toml gate, so a PR that ships an expired entry
fails before the week is out:

    python3 scripts/check_deny_expiry.py             # the repo deny.toml
    python3 scripts/check_deny_expiry.py other.toml  # any other copy
"""

from __future__ import annotations

import re
import sys
import tomllib
from datetime import date, datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_TOML = ROOT / "deny.toml"

# The expiry comment, read only inside a line's comment region (the
# text after every quoted string), never over the raw line.
EXPIRY_RE = re.compile(r"#\s*expiry:\s*(\S+)")
DATE_SHAPE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
# One `id = <quoted value>` assignment, matched over the line's masked
# code (strings replaced by placeholders), so an `id = …` written inside
# a reason string never locates an entry.
ID_ASSIGN_RE = re.compile(r"\bid\s*=\s*\x00(\d+)\x00")


class UnseenShapeError(ValueError):
    """A valid-TOML ignore entry (or ignore list) the check cannot
    structurally see. Never ridden: check() turns it into a failure row
    like every other, so the run exits 1 naming it."""


def today_utc() -> date:
    """The UTC date the expiry comparison runs against (CI runs UTC)."""
    return datetime.now(timezone.utc).date()


def split_line(line: str) -> tuple[str, list[str], str]:
    """One line → (masked code, string values, comment region).

    Quoted strings — basic ``"…"`` with escapes, literal ``'…'``, and a
    multi-line opener's same-line tail — are masked to ``\\x00<index>``
    placeholders recording their raw values in order, so nothing inside
    a string can look like code. The comment is the first ``#`` outside
    every string, and everything from it on: a ``#`` (or an
    ``expiry:``) inside a reason string is string content, never the
    entry's comment. A string opened and never closed on the line makes
    the whole tail string content — the line has no comment.
    """
    code: list[str] = []
    values: list[str] = []
    text: list[str] = []      # pending run of non-string code
    content: list[str] = []   # the currently open string's raw content
    quote = ""
    i, n = 0, len(line)
    while i < n:
        ch = line[i]
        if quote:
            if quote == '"' and ch == "\\":
                content.append(line[i:i + 2])
                i += 2
                continue
            if ch == quote:
                values.append("".join(content))
                content = []
                quote = ""
                code.append(f"\x00{len(values) - 1}\x00")
            else:
                content.append(ch)
            i += 1
            continue
        if ch in "\"'":
            code.append("".join(text))
            text = []
            quote = ch
        elif ch == "#":
            return "".join(code), values, line[i:]
        else:
            text.append(ch)
        i += 1
    code.append("".join(text))
    return "".join(code), values, ""


def _id_assignment(line: str) -> tuple[str, str] | None:
    """The line's top-level `id = <quoted value>` assignment as
    (value, comment region), or None when the line holds none — a
    masked-string match, so both quote styles locate entries and an
    `id = …` inside a reason string never does."""
    code, values, comment = split_line(line)
    m = ID_ASSIGN_RE.search(code)
    if m is None:
        return None
    return values[int(m.group(1))], comment


def parse_entries(text: str) -> list[tuple[int, str, str]]:
    """One (line number, advisory id, expiry) triple per ignore entry.

    The entry list is tomllib's [advisories].ignore — the structural
    parse, never a regex over the file for discovery — and each entry
    is tied to the line carrying its `id = <value>` assignment, matched
    in list order. The expiry is that line's comment region alone: a
    missing comment yields "" and a value that is not a date passes
    through as-is — both are failures past_due() names, never silently
    dropped entries. Raises UnseenShapeError (or tomllib's decode
    error) on a shape the check cannot see.
    """
    doc = tomllib.loads(text)
    ignores = doc.get("advisories", {}).get("ignore")
    if ignores is None:
        ignores = []
    if not isinstance(ignores, list):
        raise UnseenShapeError(
            f"[advisories] ignore is a {type(ignores).__name__}, not a list of entries")
    assignments: list[tuple[int, str, str]] = []
    for lineno, line in enumerate(text.splitlines(), start=1):
        found = _id_assignment(line)
        if found is not None:
            assignments.append((lineno, *found))
    used = [False] * len(assignments)
    entries = []
    for i, item in enumerate(ignores):
        if not isinstance(item, dict) or not isinstance(item.get("id"), str):
            raise UnseenShapeError(
                f"ignore entry {i} is {item!r}, not an entry carrying an `id` string")
        advisory = item["id"]
        for j, (lineno, value, comment) in enumerate(assignments):
            if used[j] or value != advisory:
                continue
            used[j] = True
            expiry = EXPIRY_RE.search(comment)
            entries.append((lineno, advisory, expiry.group(1) if expiry else ""))
            break
        else:
            raise UnseenShapeError(
                f"ignore entry {advisory!r} has no `id = {advisory!r}` line this check "
                "can see — inline and [[advisories.ignore]] entries, single- or "
                "double-quoted, are supported; any other shape fails loudly rather "
                "than riding unenforced")
    return entries


def past_due(entries: list[tuple[int, str, str]], today: date) -> list[tuple[int, str, str]]:
    """The failures in one entry list: (line, id, why), empty = current.

    Three ways an entry fails: its expiry has passed (the re-triage
    prompt), it carries no expiry comment, or the comment is not a
    YYYY-MM-DD date — each names itself on its own line.
    """
    failures = []
    for lineno, advisory, expiry in entries:
        if expiry == "":
            failures.append(
                (lineno, advisory, "no expiry comment — every ignore entry carries one (deny.toml [advisories] header)")
            )
            continue
        if DATE_SHAPE.match(expiry) is None:
            failures.append((lineno, advisory, f"expiry {expiry!r} is not a YYYY-MM-DD date"))
            continue
        try:
            entry_date = date.fromisoformat(expiry)
        except ValueError:
            failures.append((lineno, advisory, f"expiry {expiry!r} is not a valid calendar date"))
            continue
        if entry_date < today:
            failures.append(
                (lineno, advisory, f"expired {expiry} — re-triage: delete the entry, or rewrite it with a fresh reason and expiry")
            )
    return failures


def check(path: Path, today: date) -> list[tuple[int, str, str]]:
    """The failures in one deny.toml; empty means every entry is current."""
    text = path.read_text(encoding="utf-8")
    try:
        entries = parse_entries(text)
    except (tomllib.TOMLDecodeError, UnseenShapeError) as e:
        return [(0, "-", f"unseen shape, not enforced — {e}")]
    return past_due(entries, today)


def main(argv: list[str]) -> int:
    path = Path(argv[1]) if len(argv) > 1 else DEFAULT_TOML
    failures = check(path, today_utc())
    for lineno, advisory, why in failures:
        print(f"{path}:{lineno}: {advisory}: {why}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
