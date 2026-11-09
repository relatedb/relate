#!/usr/bin/env python3
"""Scan the tracked tree for pre-rename (CypherLite) residuals.

The rename (ADR-0517, PR #749) keeps a deliberate keep-list: frozen records
(accepted ADRs, baseline envelopes, vendored corpora, golden stores, dated
research notes, the CHANGELOG), the on-disk store artifacts whose bytes
existing stores depend on (`.cypherlite.lock`, the `.cypherlite` sidecar,
 `CYPHERLITE-LOCK-v1`), the still-set-old-env guard
(LABS-973), and pre-cutover external names (the harness repository) that
flip at the relatedb cutover. Everything else carrying the old name is a
stray the rename missed.

Every occurrence is classified and the strays are listed; exit is 1 when
any stray exists. The scan runs in CI: `make test-rename-residuals` (a
`test-scripts` prerequisite) runs the unit suite including the
tracked-tree gate, so a stray fails the review-scripts lane. The manual
forms below are the same scan against a local checkout:

    python3 scripts/rename-residuals.py            # classify + strays
    python3 scripts/rename-residuals.py --all      # also print every keep
    python3 scripts/rename-residuals.py --root DIR # scan another checkout
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from enum import Enum
from fnmatch import fnmatch
from pathlib import Path


# The closed vocabulary of occurrence classes: every keep class the
# deliberate-keep policy names, plus the two outcomes the scan itself
# assigns. Str-valued so a class compares, hashes, and sorts as its string
# value — the report lines and the test assertions use the raw strings.
class OccurrenceClass(str, Enum):
    ON_DISK_ARTIFACT = "on-disk artifact"
    RENAME_GUARD = "rename guard"
    ADR_FILENAME_REF = "ADR filename ref"
    PRE_CUTOVER_NAME = "pre-cutover name"
    STRAY = "stray"
    FROZEN_RECORD = "frozen record"


# Whole-file keeps: dated or byte-frozen records whose old name is history,
# never swept (ADR-0517 §7).
KEEP_FILE_GLOBS = (
    "docs/adr/*",
    "CHANGELOG.md",
    "baselines/*",
    "testdata/*",
    ".github/scripts/self-update-smoke*",
    ".github/scripts/tests/self-update-smoke.bats",
    "testing/jepsen/runs/*",
    # This scanner and its test name the old name by design (fixtures).
    "scripts/rename-residuals.py",
    "scripts/tests/test_rename_residuals.py",
)

# Line-level keeps, valid in any file. Each pattern is one keep class.
KEEP_LINE_PATTERNS = (
    # On-disk store artifacts (ADR-0517 §4): the bytes existing stores
    # depend on — the sidecar family, lock, writer, schema mirror.
    (OccurrenceClass.ON_DISK_ARTIFACT, re.compile(r"\.cypherlite")),
    (OccurrenceClass.ON_DISK_ARTIFACT, re.compile(r"CYPHERLITE-LOCK-v1")),
    # The rename guard's own symbol and its ADR citations.
    (OccurrenceClass.RENAME_GUARD, re.compile(r"reject_cypherlite_env")),
    (OccurrenceClass.RENAME_GUARD, re.compile(r"ADR-0517")),
    # ADR filenames keep the old name (§7); references to them are keeps.
    (OccurrenceClass.ADR_FILENAME_REF, re.compile(r"[0-9]{4}-cypherlite-")),
    # The harness repository lives under its pre-rename name until the
    # relatedb cutover moves it (ADR-0517 §3).
    (OccurrenceClass.PRE_CUTOVER_NAME, re.compile(r"cypherlite-cypher-test-harness")),
)

# Old `CYPHERLITE_*` env names, the `CYPHERLITE_` prefix literal, and the
# guard's own `cypherlite_env_*` identifiers are valid only where the
# rename guard and its tests deliberately spell them (a still-set old name
# fails closed naming its new spelling).
ENV_GUARD_FILES = (
    "relate/relate/src/open_config.rs",
    "surfaces/relate-cli/src/main.rs",
    "surfaces/relate-cli/tests/e2e_version.rs",
)
ENV_NAME = re.compile(r"cypherlite_", re.IGNORECASE)

FINDER = re.compile(r"cypherlite", re.IGNORECASE)
ENV_GUARD_CLASS = OccurrenceClass.RENAME_GUARD


def classify(path: str, line: str) -> OccurrenceClass | None:
    """The keep class for one line, 'stray' for an old-name hit that is
    none of the deliberate keeps, or None when the line carries no old
    name at all."""
    for label, rx in KEEP_LINE_PATTERNS:
        if rx.search(line):
            return label
    if ENV_NAME.search(line) and path in ENV_GUARD_FILES:
        return ENV_GUARD_CLASS
    if FINDER.search(line):
        return OccurrenceClass.STRAY
    return None


def scan(root: Path):
    """Yield (path, line_no, text, class) for every old-name occurrence."""
    out = subprocess.run(
        ["git", "grep", "-n", "-i", "-I", "cypherlite"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    for row in out.splitlines():
        parts = row.split(":", 2)
        if len(parts) != 3 or not parts[1].isdigit():
            continue
        path, line_no, text = parts
        if any(fnmatch(path, glob) for glob in KEEP_FILE_GLOBS):
            yield path, int(line_no), text, OccurrenceClass.FROZEN_RECORD
            continue
        label = classify(path, text)
        if label is not None:
            yield path, int(line_no), text, label


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    ap.add_argument("--all", action="store_true", help="print every classified occurrence, not just strays")
    args = ap.parse_args()

    counts = {}
    strays = []
    total = 0
    for path, line_no, text, label in scan(args.root):
        total += 1
        counts[label] = counts.get(label, 0) + 1
        if label == OccurrenceClass.STRAY:
            strays.append((path, line_no, text))
        elif args.all:
            print(f"keep [{label.value}] {path}:{line_no}: {text.strip()[:120]}")

    print(f"rename-residuals: {total} old-name occurrences")
    for label in sorted(counts):
        print(f"  {label.value:<18} {counts[label]}")
    for path, line_no, text in strays:
        print(f"STRAY {path}:{line_no}: {text.strip()[:120]}", file=sys.stderr)
    if strays:
        print(f"rename-residuals: {len(strays)} stray occurrences — the rename missed these", file=sys.stderr)
        return 1
    print("rename-residuals: no strays")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
