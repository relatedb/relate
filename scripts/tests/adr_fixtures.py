"""Shared fixtures for the ADR-number script suites.

The ADR suites (`test_adr_mint.py`, `test_check_adr_numbers.py`) drive
fixture git repos and run the script under test as a subprocess. They
previously carried per-file copies of the asserting git runner (the same
`assert result.returncode == 0` policy line), the script-subprocess
plumbing, the ledger machinery (the hand-built `claim` commit pushed as
the counter branch, the absent-ref probe that tells "no ledger yet" apart
from a broken repo, and the branch ref), and two constructions of the
same fixture repo — a seed repo cloned into the work repo in one suite,
init-plus-push in the other — so a policy change had to land in two
copies to keep the suites from drifting apart. The shared pieces now live
here, exactly once:

- `AdrFixture` — the one fixture construction, used by both suites: a
  bare `origin` remote plus one work repo whose seeded `main` is pushed
  to it, and the `feature` branch forked from main for the checker suite.
  The work repo is seeded in place (the old seed-repo-plus-clone round
  trip bought nothing: neither script under test touches main), and the
  repo-config identity on both repos covers the commits made without
  GIT_ENV (the pre-receive hook in the remote, the script under test in
  the work repo). Its methods are the suites' whole surface: `mint` and
  `run` run the script under test, `commit` and `claim` build the
  checker's change and ledger, and `counter_tip`/`counter_log`/
  `counter_next`/`write_counter`/`install_hook` drive the mint's remote
  ledger.
- `run_git` — the one asserting git runner. env is a required keyword
  argument (pass GIT_ENV), so the deterministic identity is enforced at
  the boundary. check=True (the default) asserts success, naming the
  failing arguments and git's stderr; check=False is for tolerant callers
  that inspect failures themselves (e.g. an absent-ref probe, which must
  still tell "ref absent" apart from a broken repo).
- `run_script` — the one run-script-under-test helper: subprocess the
  script with the current interpreter in a given cwd and return the
  CompletedProcess.
- `push_counter_commit` — the one hand-built counter-branch commit: the
  NEXT blob/tree/commit plumbing plus the push to REF, so both suites
  seed the ledger exactly as `scripts/adr_mint.py` builds and pushes one.
- `ref_tip` — the one absent-ref probe (`rev-parse --verify --quiet`):
  None only when the ref is absent, raised on a broken repo, so "no
  ledger yet" is never a guess.
- `COUNTER_BRANCH` (imported from `adr_lib`, the canonical home of the
  ledger identity) and `REF`, its full local ref, so the two suites and
  the fixture plumbing agree on one spelling.
- `MINT_SCRIPT` / `CHECK_SCRIPT` — the scripts under test, each defined
  once for the fixture methods and the suites' own calls.
- `GIT_ENV` — the deterministic author/committer identity (and dates)
  both suites share for the commits they create, so fixture history
  never depends on the host git config or the wall clock.
- `ROOT` — the repo root, resolved once from this file's location.

Imported as a plain sibling module: the Makefile runs each test file
directly with `python3`, so `sys.path[0]` is `scripts/tests/`; `scripts/`
is added to the import path the same way the scripts add their own
directory, so the ledger identity comes from its one home, `adr_lib`.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

from child_env import child_env

ROOT = Path(__file__).resolve().parents[2]

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from adr_lib import COUNTER_BRANCH, claim_subject

# The per-invocation git variables a surrounding git hook leaks into the
# suite: the script under test must see the repo the caller's shell would
# resolve, never the hook's own. Distinct vocabulary from the reporting
# pair the other suites scrub (child_env.REPORTING_ENV).
GIT_SCRUB = ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_QUARANTINE_PATH")

GIT_ENV = {
    **os.environ,
    "GIT_AUTHOR_NAME": "ADR Test",
    "GIT_AUTHOR_EMAIL": "adr@example.com",
    "GIT_COMMITTER_NAME": "ADR Test",
    "GIT_COMMITTER_EMAIL": "adr@example.com",
    "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z",
    "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z",
}

# The full local ref of the ledger branch, derived from the one
# branch-name definition in adr_lib; the suites seed the branch and read
# its tip through the helpers below.
REF = f"refs/heads/{COUNTER_BRANCH}"

MINT_SCRIPT = ROOT / "scripts" / "adr_mint.py"
CHECK_SCRIPT = ROOT / "scripts" / "check_adr_numbers.py"


def run_git(
    repo: Path,
    *args: str,
    env: dict[str, str],
    input_text: str | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Run one git command against repo; return the CompletedProcess.

    env is a required keyword argument (pass GIT_ENV): the deterministic
    identity is enforced at the boundary, so no call silently re-selects
    the host git identity. With the default check=True, assert the command
    succeeded, naming the failing arguments and git's stderr. check=False
    is for callers that inspect failures themselves; they must still
    distinguish an expected failure shape (e.g. an absent ref) from a
    broken repo.
    """
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        input=input_text,
        text=True,
        capture_output=True,
        check=False,
        env=env,
    )
    if check:
        assert result.returncode == 0, f"git {' '.join(args)}: {result.stderr}"
    return result


def run_script(
    script: Path, *args: str, cwd: Path
) -> subprocess.CompletedProcess[str]:
    """Run one scripts/ file under test as a subprocess in cwd.

    Runs with the current interpreter and the test process's environment
    minus the per-invocation git variables a surrounding git hook would
    leak (GIT_SCRUB), so the script sees the environment a caller's shell
    would give it. The scrub policy is child_env's, the one home shared
    with the reporting-env suites.
    """
    return subprocess.run(
        [sys.executable, str(script), *args],
        cwd=cwd,
        text=True,
        capture_output=True,
        check=False,
        env=child_env(GIT_SCRUB),
    )


def push_counter_commit(
    work_repo: Path,
    next_value: int,
    message: str,
    parent: str | None = None,
) -> str:
    """Push the next hand-built counter commit from work_repo; return its SHA.

    Builds the NEXT blob holding ``<next_value:04d>`` (hash-object -w
    --stdin), wraps it in a one-entry tree (mktree), and commits it
    (commit-tree, parented on ``parent`` when given) — the plumbing shape
    scripts/adr_mint.py uses for its claim commits. The commit is pushed
    straight to REF, so the worktree and the index are never touched.
    work_repo must have the bare remote as ``origin``.
    """
    blob = run_git(
        work_repo,
        "hash-object",
        "-w",
        "--stdin",
        input_text=f"{next_value:04d}\n",
        env=GIT_ENV,
    ).stdout.strip()
    tree = run_git(
        work_repo,
        "mktree",
        input_text=f"100644 blob {blob}\tNEXT\n",
        env=GIT_ENV,
    ).stdout.strip()
    args = ["commit-tree", tree] + (["-p", parent] if parent else [])
    commit = run_git(work_repo, *args, "-m", message, env=GIT_ENV).stdout.strip()
    run_git(work_repo, "push", "-q", "origin", f"{commit}:{REF}", env=GIT_ENV)
    return commit


def ref_tip(repo: Path, ref: str) -> str | None:
    """The tip commit of ref in repo; None when the ref does not exist.

    rev-parse --verify --quiet reports an absent ref by exiting 1 with
    empty output; any other failure (broken repo, missing git) raises
    instead of being mistaken for an absent branch.
    """
    result = run_git(
        repo, "rev-parse", "--verify", "--quiet", ref, env=GIT_ENV, check=False
    )
    if result.returncode == 0:
        return result.stdout.strip()
    assert (
        result.returncode == 1 and not result.stdout and not result.stderr
    ), f"git rev-parse --verify --quiet {ref}: {result.stderr}"
    return None


class AdrFixture:
    """A bare `origin` remote and one work repo whose seeded main is pushed.

    One construction for both suites: `adrs` names the ADR files seeded on
    main next to the decoy names that must not count, the seed is committed
    and pushed as `origin/main` (no seed-repo-plus-clone round trip), and
    `feature` is created forked from main for the checker suite while HEAD
    stays on main — the branch the mint suite asserts is untouched.
    """

    def __init__(self, base: Path, adrs: tuple[str, ...]) -> None:
        self.remote = base / "remote.git"
        run_git(
            base, "init", "--bare", "--initial-branch=main", str(self.remote),
            env=GIT_ENV,
        )
        # Repo-config identity on the remote and the work repo covers what
        # runs without GIT_ENV: the pre-receive hook commits in the remote,
        # and the script under test commits in the work repo. Everywhere
        # else GIT_ENV is the identity.
        run_git(self.remote, "config", "user.name", "ADR Fixture", env=GIT_ENV)
        run_git(self.remote, "config", "user.email", "adr-fixture@example.com", env=GIT_ENV)
        self.work = base / "work"
        self.work.mkdir()
        run_git(self.work, "init", "-q", "-b", "main", env=GIT_ENV)
        run_git(self.work, "config", "user.name", "ADR Fixture", env=GIT_ENV)
        run_git(self.work, "config", "user.email", "adr-fixture@example.com", env=GIT_ENV)
        run_git(self.work, "remote", "add", "origin", str(self.remote), env=GIT_ENV)
        adr_dir = self.work / "docs" / "adr"
        adr_dir.mkdir(parents=True)
        for name in (
            *adrs,
            "README.md",  # no number prefix: must not count
            "12345-five-digits.md",  # five digits: must not count
            "0099-not-markdown.txt",  # not .md: must not count
        ):
            (adr_dir / name).write_text(f"# {name}\n")
        run_git(self.work, "add", ".", env=GIT_ENV)
        run_git(self.work, "commit", "-q", "-m", "seed main with ADRs", env=GIT_ENV)
        run_git(self.work, "push", "-q", "-u", "origin", "main", env=GIT_ENV)
        run_git(self.work, "branch", "feature", env=GIT_ENV)

    def mint(
        self, *args: str, cwd: Path | None = None
    ) -> subprocess.CompletedProcess[str]:
        """Run scripts/adr_mint.py in the work repo, as a caller would."""
        return run_script(MINT_SCRIPT, *args, cwd=cwd or self.work)

    def run(
        self, *args: str, cwd: Path | None = None
    ) -> subprocess.CompletedProcess[str]:
        """Run scripts/check_adr_numbers.py in the work repo, as a caller would."""
        return run_script(CHECK_SCRIPT, *args, cwd=cwd or self.work)

    def commit(self, branch: str, files: dict[str, str | None]) -> None:
        """Check out branch and commit files onto it (the checker suite).

        A ``None`` value deletes the path, so one call carries the mixed
        add+delete change a re-slotting rename is — no suite re-rolls the
        git ceremony beside the fixture's own.
        """
        run_git(self.work, "checkout", "-q", branch, env=GIT_ENV)
        self._write_and_commit(files)

    def _write_and_commit(self, files: dict[str, str | None]) -> None:
        for rel, text in files.items():
            path = self.work / rel
            if text is None:
                path.unlink()
                continue
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        run_git(self.work, "add", "-A", env=GIT_ENV)
        run_git(self.work, "commit", "-q", "-m", "fixture commit", env=GIT_ENV)

    def claim(self, number: str) -> str:
        """Push a claim commit for number to the remote ledger; return its SHA.

        Mirrors scripts/adr_mint.py through the shared plumbing
        (push_counter_commit): a NEXT blob/tree/commit built with plumbing
        and pushed as the ledger branch, without touching the worktree or
        the index. The subject comes from adr_lib.claim_subject, the one
        home of the format. The fetch afterwards materializes the
        remote-tracking ref the checker reads.
        """
        commit = push_counter_commit(
            self.work,
            int(number) + 1,
            claim_subject(int(number)),
            parent=self.counter_tip(),
        )
        run_git(self.work, "fetch", "-q", "origin", env=GIT_ENV)
        return commit

    def counter_tip(self) -> str | None:
        """Tip of the remote counter branch; None when it does not exist."""
        return ref_tip(self.remote, REF)

    def counter_log(self) -> list[str]:
        """Commit subjects on the remote counter branch, newest first."""
        return run_git(
            self.remote, "log", "--format=%s", REF, env=GIT_ENV
        ).stdout.splitlines()

    def counter_next(self) -> str:
        """The NEXT file on the remote counter branch, stripped."""
        return run_git(self.remote, "show", f"{REF}:NEXT", env=GIT_ENV).stdout.strip()

    def write_counter(self, next_value: int, message: str) -> None:
        """Hand-craft the remote counter branch (shared plumbing, pushed)."""
        push_counter_commit(
            self.work, next_value, message, parent=self.counter_tip()
        )

    def install_hook(self, name: str, body: str) -> None:
        """Install an executable hook in the bare remote."""
        hook = self.remote / "hooks" / name
        hook.write_text(body)
        hook.chmod(0o755)
