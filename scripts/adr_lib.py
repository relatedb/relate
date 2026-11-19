#!/usr/bin/env python3
"""Shared git plumbing for the ADR number scripts.

`adr_mint.py` (claims the next ADR number on the shared counter branch)
and `check_adr_numbers.py` (PR check that a change introduces no
conflicting ADR number) both run git subprocesses, anchor themselves to
the repository root, match ADR file names, and list docs/adr/. Those
definitions live here so the two scripts cannot drift:

- one git runner (`git`) and its checked variant (`checked_git`) with a
  single failure policy: a failed checked command dies exit 1 — a mid-run
  infrastructure failure — through the one die-with-git-stderr rendering
  (`die_with_git_stderr`). Caller-side usage errors keep exit 2 through
  argparse at the call site (a `type=` callable raising ArgumentTypeError,
  or parser.error), not through this module;
- `benign_rc1`, the one return-code triage for probe calls whose
  exit 1 is an expected negative answer, not a failure — an
  unresolvable `rev-parse --verify --quiet`, a `merge-base
  --is-ancestor` no: True for the benign rc 1, an infrastructure
  die for any other nonzero rc, so no probe re-derives which exit
  codes mean what;
- `anchor_repo_root`, so docs/adr pathspecs ignore the invocation cwd;
- the single ADR-filename acceptance (`adr_number`), matched against the
  basename: ``NNNN-<slug>.md`` with a non-empty slug;
- `list_adr_paths`, a NUL-safe (`-z`) ls-tree of docs/adr/ on a rev;
- `adr_entries`, the one parse-and-filter that turns a NUL-split git path
  list into (number, path) ADR entries, dropping non-matches;
- the ledger identity (`COUNTER_BRANCH`, `COUNTER_REF`) and the one
  claim-commit subject format (`claim_subject`), so the mint, the checker
  and the fixture suites cannot drift on where claims live or what a
  claim commit's subject looks like.
"""

from __future__ import annotations

import os
import re
import subprocess
from pathlib import PurePosixPath
from typing import NoReturn

ADR_DIR = "docs/adr/"
ADR_NAME = re.compile(r"^(\d{4})-.+\.md$")

# The shared ledger branch scripts/adr_mint.py claims numbers on and
# scripts/check_adr_numbers.py proves claims against, plus the
# remote-tracking ref the checker reads (the CI job fetches the branch
# under that ref before the check runs; the checker never talks to the
# network itself).
COUNTER_BRANCH = "ADR-NUMBER-MINT-DO-NOT-DELETE"
COUNTER_REF = f"origin/{COUNTER_BRANCH}"


def claim_subject(number: int) -> str:
    """The ledger commit subject for a claimed number: 'claim NNNN'."""
    return f"claim {number:04d}"


def git(*args: str, input_text: str | None = None) -> subprocess.CompletedProcess[str]:
    """Run one git command in the current directory; never raises."""
    return subprocess.run(
        ["git", *args],
        input=input_text,
        text=True,
        capture_output=True,
        check=False,
    )


def die(program: str, message: str) -> NoReturn:
    """Exit 1 with '<program>: <message>' on stderr: infrastructure failure."""
    raise SystemExit(f"{program}: {message}")


def die_with_git_stderr(
    program: str,
    message: str,
    result: subprocess.CompletedProcess[str],
) -> NoReturn:
    """Die with the caller's message and git's stderr: the one rendering
    of a failed git command's exit-1 die, shared by `checked_git` and
    `benign_rc1` — the caller's message first, git's stripped stderr
    appended after a ': ' when the command produced any.
    """
    detail = result.stderr.strip()
    die(program, f"{message}: {detail}" if detail else message)


def checked_git(
    program: str,
    message: str,
    *args: str,
    input_text: str | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run one git command; on failure die exit 1 with message and git stderr.

    This is the mid-run infrastructure failure path: the git subprocess
    itself could not do what the script needed. Caller-side usage errors
    keep exit 2 through argparse at the call site (a `type=` callable
    raising ArgumentTypeError, or parser.error) and never reach here.
    """
    result = git(*args, input_text=input_text)
    if result.returncode != 0:
        die_with_git_stderr(program, message, result)
    return result


def benign_rc1(
    program: str,
    message: str,
    result: subprocess.CompletedProcess[str],
) -> bool:
    """True when <result> is the benign rc-1 negative, False when rc 0.

    The one return-code triage for git probes whose exit 1 is an
    expected negative answer, not a failure: `rev-parse --verify
    --quiet` on a name that does not resolve, `merge-base
    --is-ancestor` on an unrelated pair. The caller answers its
    user-facing question when True and proceeds when False; any other
    nonzero rc is a mid-run infrastructure failure, checked_git's own
    policy, and dies exit 1 with <message> and git's stderr — never a
    misreported negative.
    """
    if result.returncode == 1:
        return True
    if result.returncode != 0:
        die_with_git_stderr(program, message, result)
    return False


def anchor_repo_root(program: str) -> None:
    """cd to the repo root so docs/adr pathspecs ignore the invocation cwd."""
    result = checked_git(
        program, "cannot resolve the repository root", "rev-parse", "--show-toplevel"
    )
    os.chdir(result.stdout.strip())


def adr_number(path: str) -> str | None:
    """The NNNN prefix of a docs/adr/NNNN-<slug>.md path, else None.

    Matches the basename only, so the same acceptance applies to full
    paths from ls-tree and to relative paths from diff.
    """
    match = ADR_NAME.match(PurePosixPath(path).name)
    return match.group(1) if match else None


def nul_split(text: str) -> list[str]:
    """Paths from -z git output: NUL-terminated, never C-quoted."""
    return [field for field in text.split("\0") if field]


def adr_entries(paths: list[str]) -> list[tuple[str, str]]:
    """(number, path) pairs for the ADR files in a NUL-split git path list.

    Applies `adr_number` to each path's basename and drops non-matches, so
    the parse-and-filter policy exists exactly once for every caller.
    """
    pairs = []
    for path in paths:
        number = adr_number(path)
        if number is not None:
            pairs.append((number, path))
    return pairs


def list_adr_paths(program: str, rev: str) -> list[str]:
    """Full paths under docs/adr/ on <rev>, listed NUL-safe with -z."""
    result = checked_git(
        program,
        f"cannot list {ADR_DIR} on {rev}",
        "ls-tree",
        "--name-only",
        "-z",
        rev,
        "--",
        ADR_DIR,
    )
    return nul_split(result.stdout)
