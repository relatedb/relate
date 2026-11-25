#!/usr/bin/env python3
"""Fold a crate's integration-test files into one test binary.

Every file directly under `<crate>/tests/` is its own Cargo test target:
its own rustc invocation and its own static link of the whole engine. For
`relate/relate` that is ~145 links of a ~450 MB debug binary. This script
moves the files that can share a process into `<crate>/tests/<binary>/`,
which Cargo builds as one target from `<binary>/main.rs`.

A file stays a binary of its own when sharing a process would change
what it tests. `ISOLATION_PATTERNS` holds the rules a text scan can see —
comment lines are blanked first so a rule named in prose does not count,
except the preview rule, which reads them (see the table) — and
`KEEP_ALWAYS` records the files kept for a reason no scan can see;
Files the scan keeps separate
are reported. The move is idempotent: files already under `<binary>/` are
left alone and `main.rs` is regenerated from scratch. `main.rs` is
generated output, not an editable file — a hand edit to it fails
`scripts/tests/test_merge_test_binaries.py`, which holds each committed
`main.rs` equal to what this script renders, so a bare re-run reproduces
the committed layout.

The generated `<binary>/main.rs` carries one guard test per directory — a
call into `relate_test_support::assert_every_file_is_declared`, the
guard's one home — and `scripts/tests/test_merge_test_binaries.py`
(pinned by the `test-merge-test-binaries` Makefile target) fails if a
committed `main.rs` drifts from what this script renders.

Usage: scripts/merge-test-binaries.py <crate-dir> [--dry-run]
"""
import argparse
import re
import subprocess
import sys
import textwrap
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Rule:
    """One isolation rule: what it matches, and where it reads.

    `reads_comments` is the rule's own property (required, so no row can
    inherit a subject silently), so the preview rule's comment-reading
    cannot be lost by rewording the table key.
    """

    pattern: re.Pattern[str]
    reads_comments: bool


# A file is kept separate when its text matches any of these. Each key is a
# whole phrase so `render_main` can state the policy in the generated header
# from this one table.
ISOLATION_PATTERNS = {
    # `RELATE_PREVIEW` is read once at open and `set_preview_once` sets it
    # for the whole process (a permanent `Once`, never restored), so a
    # sibling test would see the gate open — or, for a closed-gate fixture,
    # see it open and stop failing closed. The helper names are the ones a
    # caller spells instead of the knob: the enabling constructor and the
    # closed-side assertion / expected-text helpers.
    "it reads or sets the process-wide preview opt-in": Rule(
        re.compile(
            r"\bRELATE_PREVIEW\b|\bset_preview_once\b|\bpreview_ontology\b"
            r"|\bassert_preview_closed\b|\bassert_preview_unset\b"
            r"|\bshow_variants_connection\b|\bgated_off_error\b"
            r"|\bpreview_feature_disabled_text\b"
        ),
        reads_comments=True,
    ),
    # `BatchEnv` is the bulk-batch window pinner (`RELATE_BULK_BATCH_*`); it
    # holds the env seam, but a sibling test that opens a `Database` directly
    # (not through `common::env_seam`) would read the pinned values, so a
    # pinning file stays separate like the engine-pin helpers beside it.
    "it mutates the process environment": Rule(
        re.compile(
            r"\bset_var\b|\bremove_var\b|\bopen_with_engine\b|\bfor_each_engine\b"
            r"|\bEnvRestore\b|\bBatchEnv\b"
        ),
        reads_comments=False,
    ),
    "it samples process-wide memory": Rule(
        re.compile(r"\brss\b|\bRSS\b|global_allocator"), reads_comments=False
    ),
    "it re-executes its own binary": Rule(
        re.compile(r"current_exe\(\)"), reads_comments=False
    ),
    "it owns the test crate root": Rule(
        re.compile(r"\bcrate::|#\[path\s*="), reads_comments=False
    ),
}

# Files that must stay their own binary for a reason no text scan can see,
# keyed by repo-relative crate dir so a bare re-run reproduces the
# committed layout.

# The one spelling of the shared binary's name (the guard reads it too).
DEFAULT_BINARY = "integration"
KEEP_ALWAYS = {
    "relate/relate": {
        "golden_stores": "copied into an old release checkout and compiled "
        "standalone there (testdata/stores/README.md)",
    },
}

def strip_comment_lines(text: str) -> str:
    """Blank out comments and the merge's own `use crate::<helper>;` lines, so
    a rule named in prose (a doc comment) does not keep a file separate —
    surfaces/relate-cli's e2e_upgrade.rs mentions `current_exe()` only in a
    doc comment. Deliberately conservative: it never removes a line that
    could hold code, so a real call site always still matches.

    One rule: each line is cut at the first `//` no quote precedes, so a
    `//`-leading comment line is cut at its start and a `//` inside a string
    literal (a URL) is never mistaken for one. The reduction is line-based:
    a `/*` block-comment line counts as code, which is the over-keeping
    (safe) direction — a line-local arm could not tell that the following
    lines were still inside the block. The generated
    `use crate::<helper>;` line the merge itself inserts is dropped here, so
    the "owns the test crate root" rule means the file's own `crate::` uses,
    not the helper import the move introduced.
    """
    out = []
    for line in text.splitlines():
        idx = line.find("//")
        if idx != -1 and '"' not in line[:idx]:
            line = line[:idx]
        out.append(line)
    return GENERATED_USE.sub("", "\n".join(out))


# The line the merge inserts for a shared helper module
# (`use crate::common;` / `use crate::support;`).
GENERATED_USE = re.compile(r"^\s*use crate::[A-Za-z0-9_]+;\s*$", re.M)


def shared_module_pattern(name: str) -> re.Pattern[str]:
    """The per-file declaration of a shared helper module, with whatever
    attribute or doc lines sit above it.

    The whole block is replaced by the generated `use crate::<name>;`: the
    per-file `#[allow(dead_code)]` that used to silence a partially-used
    helper module has no meaning in the merged binary, where the module is
    declared once (with its own allow) in `main.rs`.
    """
    return re.compile(rf'(?:#\[[^\n]*\]\n|///[^\n]*\n)*mod {name};\n', re.M)


# A file-relative include path: one directory deeper after the move.
RELATIVE_INCLUDE = re.compile(r'(include_(?:str|bytes)!\(\s*")(?!/)')


def fold_file(text: str, shared: list[str]) -> str:
    """One test file's text as it lands in `tests/<binary>/`: relative
    include paths one directory deeper, and each shared helper module's
    `mod` declaration replaced by the generated `use crate::<name>;`."""
    text = RELATIVE_INCLUDE.sub(r"\g<1>../", text)
    for name in shared:
        text = shared_module_pattern(name).sub(f"use crate::{name};\n", text, count=1)
    return text


def isolation_reason(text: str) -> str | None:
    code = strip_comment_lines(text)
    for reason, rule in ISOLATION_PATTERNS.items():
        if rule.pattern.search(text if rule.reads_comments else code):
            return reason
    return None


def render_main(*, modules: list[str], shared: list[str]) -> str:
    binary = DEFAULT_BINARY
    policy = (
        "; ".join(ISOLATION_PATTERNS)
        + "; or it is listed in KEEP_ALWAYS"
    )
    lines = [
        f"//! The `{binary}` integration-test binary: every test file that can",
        "//! share a process with its siblings, compiled and linked once instead",
        "//! of once per file.",
    ]
    lines += [
        f"//! {line}"
        for line in textwrap.wrap(
            "A file stays a binary of its own directly under `tests/` when sharing "
            f"a process would change what it tests: {policy} "
            "(scripts/merge-test-binaries.py, the one table).",
            width=76,
            break_on_hyphens=False,
        )
    ]
    lines += [
        "//! Add a new shareable test file here as a `mod` line; the",
        "//! `every_file_is_declared` test fails on a file left out.",
        "",
    ]
    for name in shared:
        # The shared helper module declares its own dead-code policy
        # (`#![allow(dead_code)]` inside it, since each including test crate
        # uses a different subset of its helpers); emitting an allow here too
        # would be a duplicated attribute under clippy's `-D warnings`.
        lines += [f'#[path = "../{name}/mod.rs"]', f"mod {name};"]
    if shared:
        lines.append("")
    for name in modules:
        lines.append(f"mod {name};")
    lines += [
        "",
        "/// A file under this directory that no `mod` line declares would",
        "/// silently never compile or run.",
        "#[test]",
        "fn every_file_is_declared() {",
        "    relate_test_support::test_binaries::assert_every_file_is_declared(",
        '        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),',
        f'        "{binary}",',
        "    );",
        "}",
        "",
    ]
    return "\n".join(lines)


# Historical records keep the path they were written with.
FROZEN_PREFIXES = ("docs/adr/", "CHANGELOG")


def run(argv: list[str], *, cwd: Path | None = None, ok: tuple[int, ...] = (0,)) -> str:
    """Run one external command and return its stdout.

    Every subprocess this script runs goes through here, so a failure names
    the command and its stderr instead of raising a bare
    `CalledProcessError` (or `FileNotFoundError` for a missing tool), and a
    tolerated non-zero status is stated by the caller (`git grep` exits 1
    with no matches).
    """
    try:
        result = subprocess.run(argv, capture_output=True, text=True, cwd=cwd)
    except OSError as error:
        raise SystemExit(f"{argv[0]}: {error}")
    if result.returncode not in ok:
        raise SystemExit(
            f"{' '.join(argv)} failed ({result.returncode}): {result.stderr.strip()}"
        )
    return result.stdout


def repo_root(from_dir: Path) -> Path:
    """The work-tree root, resolved from `from_dir` so an absolute
    `<crate-dir>` works from any cwd. `run` owns the failure
    message — a missing git or a dubious-ownership refusal keeps its
    command and stderr instead of being relabeled."""
    return Path(run(["git", "rev-parse", "--show-toplevel"], cwd=from_dir).strip())


def crate_key(crate_dir: Path, root: Path) -> str:
    """The repo-relative `crate_dir`, so an absolute or `./`-spelled path
    behaves like the documented relative one (KEEP_ALWAYS and the
    reference-rewrite pattern both key off this). The caller resolves the
    root once; one call resolves it once."""
    try:
        return crate_dir.resolve().relative_to(root).as_posix()
    except ValueError:
        raise SystemExit(f"{crate_dir} is outside the repository at {root}")


@dataclass(frozen=True)
class MentionScan:
    """One derivation of "which tracked files mention a moved stem at its
    pre-move path" — the listing pattern, the exact substitution (the
    listing plus the `\b` extension pin, which `git grep`'s ERE cannot
    carry), the frozen-filtered mention files, and the merged destination
    the rewrite writes. Both the rewrite and the drift guard consume this,
    so the two cannot drift apart. The stem set is non-empty: the caller
    owns the nothing-moved case."""

    tests_rel: Path
    """The crate's pre-move test directory, repo-relative (`<crate>/tests`)
    — the spelling the `git grep` listing and the substitution share. A
    filesystem `Path` (like `Plan.tests_dir`) cannot be substituted for it."""
    target: Path
    """The merged destination directory, repo-relative
    (`<crate>/tests/<DEFAULT_BINARY>`)."""
    substitution: re.Pattern[str]
    files: list[Path]


def mention_scan(crate_dir: Path, root: Path, stems: list[str]) -> MentionScan:
    """The moved stems' mention scan; the stem set is non-empty (the caller
    owns the nothing-moved case, so no scan object ever carries an
    impossible pattern)."""
    tests_rel = Path(crate_key(crate_dir, root)) / "tests"
    assert stems, "mention_scan needs at least one moved stem"
    prefix = f"{tests_rel.as_posix()}/"
    # The listing is the substitution minus the `\b` pin: git's ERE does not
    # support `\b`, and a listing that carried it silently matched nothing
    # (observed: a rebuild left every doc mention on the pre-move path).
    listing = re.escape(prefix) + "(" + "|".join(map(re.escape, stems)) + r")\.rs"
    # `-z` and the NUL split: `git grep -l` emits one path per line, and a
    # whitespace split would turn a spaced or quoted path into two bogus
    # entries. The durable no-stale-mentions check is the guard suite's.
    listed = run(["git", "grep", "-l", "-z", "-E", listing], cwd=root, ok=(0, 1))
    names = [name for name in listed.split("\0") if name]
    return MentionScan(
        tests_rel=tests_rel,
        target=tests_rel / DEFAULT_BINARY,
        substitution=re.compile(listing + r"\b"),
        files=[
            root / name for name in names if not name.startswith(FROZEN_PREFIXES)
        ],
    )


def rewrite_references(crate_dir: Path, stems: list[str]) -> list[Path]:
    """Point every tracked mention of a moved file at its new path.

    Anchored at the work-tree root so the grep and the path reads do not
    depend on the caller's cwd, and a `git grep` failure is loud rather
    than an empty match set. Returns the files rewritten, for staging.
    """
    if not stems:
        return []
    root = repo_root(crate_dir)
    scan = mention_scan(crate_dir, root, stems)
    rewritten = []
    for path in scan.files:
        text = path.read_text()
        new = scan.substitution.sub(
            lambda m: (scan.target / f"{m.group(1)}.rs").as_posix(), text
        )
        if new != text:
            path.write_text(new)
            rewritten.append(path)
            print(f"rewrote references in {path.relative_to(root)}")
    return rewritten


@dataclass(frozen=True)
class Plan:
    """The layout a run would produce for one crate — the one derivation
    `main` executes and `scripts/tests/test_merge_test_binaries.py` checks,
    so the guard cannot drift from the producer."""

    tests_dir: Path
    """The crate's pre-move test directory, caller-relative — the layout
    fact the guard reads (distinct from `MentionScan.tests_rel`, which is
    repo-relative)."""
    target: Path
    moved: list[Path]
    kept: list[tuple[str, str]]
    existing: list[str]
    modules: list[str]
    shared: list[str]


def plan(crate_dir: Path) -> Plan:
    tests = crate_dir / "tests"
    target = tests / DEFAULT_BINARY
    keep_always = KEEP_ALWAYS.get(crate_key(crate_dir, repo_root(crate_dir)), {})
    moved, kept = [], []
    for path in sorted(tests.glob("*.rs")):
        stem = path.stem
        # A recorded row always wins, decided on absence (`is None`), never
        # on truthiness. Every row names a reason — the guard asserts that —
        # so the empty-string case is a table defect, not a silent move.
        reason = keep_always.get(stem)
        if reason is None:
            reason = isolation_reason(path.read_text())
        if reason is not None:
            kept.append((stem, reason))
        else:
            moved.append(path)
    # Both enumerations tolerate a missing directory the same way: `glob`
    # yields nothing when the receiver is not a directory, so the first run
    # (no target yet, or no shared helpers) needs no absence checks.
    shared = sorted(p.parent.name for p in tests.glob("*/mod.rs"))
    existing = sorted(p.stem for p in target.glob("*.rs") if p.stem != "main")
    return Plan(
        tests_dir=tests,
        target=target,
        moved=moved,
        kept=kept,
        existing=existing,
        modules=sorted({p.stem for p in moved} | set(existing)),
        shared=shared,
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("crate_dir", type=Path)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    layout = plan(args.crate_dir)
    for stem, reason in layout.kept:
        print(f"keep  {stem}: {reason}")
    print(
        f"merge {len(layout.moved)} new + {len(layout.existing)} existing "
        f"files into tests/{DEFAULT_BINARY}/; "
        f"{len(layout.kept)} stay separate"
    )
    if args.dry_run:
        return 0

    # Probe the external tools the run needs before mutating anything: a
    # missing rustfmt must fail on an untouched tree, not halfway through the
    # moves with the index already staged.
    for tool in ("git", "rustfmt"):
        run([tool, "--version"])

    target = layout.target
    target.mkdir(exist_ok=True)
    for path in layout.moved:
        dest = target / path.name
        # Read before the move: `git mv` removes the source path.
        text = fold_file(path.read_text(), layout.shared)
        run(["git", "mv", str(path), str(dest)])
        dest.write_text(text)
    (target / "main.rs").write_text(render_main(modules=layout.modules, shared=layout.shared))
    rewritten = rewrite_references(args.crate_dir, [p.stem for p in layout.moved])
    # The include-path rewrite can push a line past rustfmt's width.
    run(["rustfmt", "--edition", "2021", *map(str, sorted(target.glob("*.rs")))])
    run(["git", "add", str(target), *map(str, rewritten)])
    return 0


if __name__ == "__main__":
    sys.exit(main())
