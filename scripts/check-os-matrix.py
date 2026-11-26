#!/usr/bin/env python3
"""Check macOS and Windows CI jobs stay on filesystem depth and surface smokes.

Reads a GitHub Actions workflow. The default path is `.github/workflows/ci.yml`;
the first argument overrides it.

Exits 0 when every job whose `runs-on` or strategy matrix includes a
non-Linux runner label (the `FAMILIES` table below) satisfies all of the
following:

- cargo `-p` / `--package` arguments, tokenized, do not select `relate`,
  `relate-planner`, or `relate-executor`
- `relate-storage` is not the package `relate`; `relate-ffi`,
  `relate-mcp`, `relate-bolt`, `relate-http`, and `relate-cli`
  are allowed
- across the jobs for that OS, both filesystem cargo commands appear as
  their own script lines, and every surface-smoke command for that OS
  appears as its own script line, including `source scripts/ffi-lib.sh` and
  `bash scripts/ffi-compile-example.sh`. One OS may split those across a
  filesystem job and a surface job. The OS-independent client smokes
  (Python wheel, npm, Maven) are required on macOS only: the Windows
  surface lane keeps the compile/link checks and drops clients the ubuntu
  lanes already cover (ADR-0251 §2)
- no script line uses a `test -f` prefix
- no step assigns `librelate_ffi.so`

Exits non-zero otherwise, including when a Windows step's run is
`cargo test -p relate` or contains `-p relate-planner`.
"""

from __future__ import annotations

import shlex
import sys
from collections.abc import Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import NamedTuple

import yaml

# Runner labels per OS. A job may schedule on any label of its OS — the
# macOS lanes moved to the org's arm64 runner, older workflows still use
# the GitHub-hosted label — and the required-command coverage below is per
# OS, not per label.
FAMILIES = {
    "macos": ("macos-latest", "macos-5-core-arm64"),
    "windows": ("windows-16-core",),
}
LABEL_FAMILY = {
    label: family for family, labels in FAMILIES.items() for label in labels
}

# Exact package names. Match tokens, not substrings: `relate-storage`
# contains the letters of `relate` and must still be allowed.
FORBIDDEN_PACKAGES = frozenset(
    {
        "relate",
        "relate-planner",
        "relate-executor",
    }
)

FILESYSTEM = (
    "cargo test -p relate-storage",
    "cargo test -p relate-storage --no-default-features",
)

# Required on both non-Linux OSes: the one-query surface smokes and the
# native compile/link checks (C example via the platform compiler, cgo).
SURFACE = (
    "cargo test -p relate-mcp --test stdio_tools stdio_namespace_prefixes_tool_names -- --exact",
    "cargo test -p relate-bolt --lib server::tests::bolt_return_one -- --exact",
    "cargo test -p relate-http --lib query_api::tests::success_envelope_has_fields_values_and_empty_bookmarks -- --exact",
    "cargo test -p relate-cli --test integration e2e_bundles::test_one_shot_query_runs_in_every_bundle -- --exact",
    "source scripts/ffi-lib.sh",
    "bash scripts/ffi-compile-example.sh",
    "go test -C clients/go -count=1 -run '^TestReturnOne$' .",
)

# OS-independent client smokes required per OS on top of SURFACE. The
# Windows surface lane drops them (ADR-0251 §2): the Python wheel link is
# covered by the macOS lane and the ubuntu `python` job, and the JS/Java
# clients by the ubuntu `relate-clients` lane.
SURFACE_EXTRA = {
    "macos": (
        "python -m pytest surfaces/relate-python/python/tests/test_native.py::test_execute_returns_query_result",
        "(cd clients/javascript && npm install && node test.js)",
        "mvn -q -f clients/java/pom.xml -Drelate.ffi.dir=\"$RELATE_LIB_DIR\" '-Dtest=DatabaseTest#returnOne,Native*Test' verify",
    ),
    "windows": (),
}


def iter_strings(value: object) -> Iterator[str]:
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for key, item in value.items():
            if isinstance(key, str):
                yield key
            yield from iter_strings(item)
    elif isinstance(value, list):
        for item in value:
            yield from iter_strings(item)


def families_in(text: str) -> set[str]:
    """OS families whose runner labels are named in one runs-on or matrix string."""
    return {LABEL_FAMILY[name] for name in LABEL_FAMILY if name in text}


def non_linux_targets(job: dict) -> set[str]:
    """OS families this job actually schedules on macOS or Windows runners."""
    runs_on = job.get("runs-on")
    texts: list[str] = []
    if isinstance(runs_on, str):
        texts.append(runs_on)
    elif isinstance(runs_on, list):
        texts.extend(item for item in runs_on if isinstance(item, str))
    if "${{" in "\n".join(texts):
        strategy = job.get("strategy")
        matrix = strategy.get("matrix") if isinstance(strategy, dict) else None
        texts.extend(iter_strings(matrix))
    return families_in("\n".join(texts))


class RunLines(NamedTuple):
    """One job's `run` scripts and their stripped, non-empty lines."""

    lines: list[str]
    scripts: list[str]


def run_lines(job: dict) -> RunLines:
    lines: list[str] = []
    scripts: list[str] = []
    for step in job.get("steps") or []:
        if not isinstance(step, dict):
            continue
        run = step.get("run")
        if not isinstance(run, str):
            continue
        scripts.append(run)
        for line in run.splitlines():
            stripped = line.strip()
            if stripped:
                lines.append(stripped)
    return RunLines(lines=lines, scripts=scripts)


def cargo_packages(script: str) -> list[str]:
    """Package names passed to cargo via `-p`, `--package`, or `--package=`."""
    tokens = iter(shlex.split(script, posix=True))
    packages: list[str] = []
    for token in tokens:
        if token in ("-p", "--package"):
            value = next(tokens, None)
            if value is not None:
                packages.append(value)
        elif token.startswith(("-p=", "--package=")):
            packages.append(token.split("=", 1)[1])
    return packages


# (needle, message) pairs; one loop over lines checks them all. A needle
# never spans a line, so per-line scan is what the old blob re-join checked.
FORBIDDEN = (
    ("test -f", "test -f does not fail the step"),
    ("librelate_ffi.so", "assigns librelate_ffi.so"),
)


def check_job(name: str, run: RunLines) -> list[str]:
    """Per-job safety checks. Required commands are collected for the OS union."""
    errors: list[str] = []
    for script in run.scripts:
        try:
            packages = cargo_packages(script)
        except ValueError as exc:
            errors.append(f"{name}: could not tokenize run script: {exc}")
            continue
        for package in packages:
            if package in FORBIDDEN_PACKAGES:
                errors.append(f"{name}: step run selects -p {package}")
    for line in run.lines:
        for needle, message in FORBIDDEN:
            if needle in line:
                errors.append(f"{name}: {message}: {line}")
    return errors


@dataclass
class Coverage:
    """What one non-Linux OS family covers: job names and their script lines."""

    jobs: list[str] = field(default_factory=list)
    lines: list[str] = field(default_factory=list)


REQUIRED = (
    ("filesystem command", FILESYSTEM),
    ("surface smoke", SURFACE),
)


def check_coverage(coverage: dict[str, Coverage]) -> list[str]:
    """Per-family required-command coverage: every family schedules at
    least one job, and that family's jobs carry every required command —
    the shared set plus the family's own client smokes."""
    errors: list[str] = []
    for family, cov in coverage.items():
        if not cov.jobs:
            errors.append(f"no job runs on a {family} runner")
            continue
        present = set(cov.lines)
        for noun, required in (*REQUIRED, ("surface smoke", SURFACE_EXTRA[family])):
            for command in required:
                if command not in present:
                    errors.append(f"{family}: missing {noun}: {command}")
    return errors


def main(argv: list[str]) -> int:
    path = Path(argv[1] if len(argv) > 1 else ".github/workflows/ci.yml")
    if not path.is_file():
        print(f"workflow not found: {path}", file=sys.stderr)
        return 1
    try:
        document = yaml.safe_load(path.read_text())
    except yaml.YAMLError as exc:
        print(f"failed to parse {path}: {exc}", file=sys.stderr)
        return 1
    jobs = document.get("jobs") if isinstance(document, dict) else None
    if not isinstance(jobs, dict):
        print(f"{path}: no jobs map", file=sys.stderr)
        return 1

    errors: list[str] = []
    coverage = {family: Coverage() for family in FAMILIES}
    for name, job in jobs.items():
        if not isinstance(job, dict):
            continue
        targets = non_linux_targets(job)
        if not targets:
            continue
        run = run_lines(job)
        errors.extend(check_job(name, run))
        for family in targets:
            coverage[family].jobs.append(name)
            coverage[family].lines.extend(run.lines)

    errors.extend(check_coverage(coverage))

    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    print(f"{path}: macOS and Windows jobs are filesystem depth and surface smoke only")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
