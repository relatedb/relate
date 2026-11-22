"""Shared comparator-subprocess harness for the scripts/tests/ suites.

`run_pair` runs `scripts/compare-loadtest.py` over a (baseline, current)
report pair — the one harness behind both comparator suites
(`test_compare_loadtest.py`, `test_compare_verdict.py`). They previously
carried near-verbatim copies with drifting semantics: the verdict copy
scrubbed `LOADTEST_*` threshold overrides from the child environment while
the loadtest copy did not, so a developer's exported
`LOADTEST_MAX_P99_RATIO=99` silently loosened one suite's bands but not
the other's. The harness lives exactly once here, with the strict
semantics: `GITHUB_ACTIONS` and every `LOADTEST_*` variable are scrubbed
(a test opts back in through `extra_env`), and the optional
`--markdown-out` / `--status-out` files are read back for the caller.

`render` is the shared render-mode shorthand over `run_pair`: it runs the
comparator with `--render-only` plus a `--markdown-out` sidecar and reads
the table back. Both comparator suites previously carried their own copy
(`RenderMarkdownTests.render` in the loadtest suite, the module-level
`render` in the verdict suite) with divergent knobs — extra flags in one,
`extra_env` in the other — that would drift apart. The one shorthand here
takes the superset: extra flags are appended after `--render-only` and the
same env scrub applies, re-addable through `extra_env`.

`run_comparator` is the one pair-write + invoke primitive both bindings
share: it materializes a (baseline, current) report pair in a temp dir and
runs a comparator over it. `run_pair` is its compare-loadtest.py binding
(positional pair arguments); the query-latency gate suite binds the same
primitive to compare-query-latency.py's `--baseline`/`--report` spelling
instead of carrying a third copy of the temp-dir-and-two-reports core.
`run_pair` returns the sidecar contents keyed by the flag that wrote them,
so a caller asks for the flags it wants out of the one PAIR_SIDECARS table
and reads the result back under the same names.

`run_memory_comparator` is the binding the two memory suites share: the
memory comparators spell the pair `--baseline`/`--report` and write their
artefacts through `--doc`/`--verdict`, so the one binding returns a
`MemoryCompare` — the process and the two artefacts, parsed — instead of
each suite grafting `.document`/`.payload` onto a `CompletedProcess` and
passing `os.environ` straight through (the drift the shared primitive
exists to end).

`run_freeze` is the one freeze/envelope harness: write N reports as
`run{i}.json`, run the freeze script with `--out`, read the payload back —
the shape `test_freeze_baseline_envelope.py`, `test_query_latency_gate.py`
and the two memory suites each carried near-verbatim.

Imported as a plain sibling module: the Makefile runs each test file
directly with `python3`, so `sys.path[0]` is `scripts/tests/`.
"""

import json
import os
import subprocess
import sys
import tempfile
from collections.abc import Callable
from pathlib import Path
from typing import NamedTuple


from child_env import child_env


ROOT = Path(__file__).resolve().parents[2]
COMPARE = ROOT / "scripts" / "compare-loadtest.py"

# Every env-overridable knob vocabulary the served comparators read,
# spelled once: the loadtest bands (LOADTEST_*) and the query-latency
# ADR-0245 bands (QUERY_LATENCY_*). A future comparator knob extends
# this tuple — never a second scrub branch.
SCRUB_PREFIXES = ("LOADTEST_", "QUERY_LATENCY_")

# compare-loadtest.py's sidecar flags -> the file each writes inside the
# run's temp dir. The one table of them: a caller names the subset it wants
# by flag (never a boolean per flag), and reads the contents back under the
# same flag names.
PAIR_SIDECARS = {"--markdown-out": "table.md", "--status-out": "status.txt"}

# The two memory comparators' sidecar flags -> the file each writes inside
# the run's temp dir: the rendered document (`--doc`) and the verdict JSON
# (`--verdict`). The one table `run_memory_comparator` binds, so neither
# memory suite re-spells the pair.
MEMORY_SIDECARS = {"--doc": "report.md", "--verdict": "verdict.json"}


class FreezeRun(NamedTuple):
    """A freeze/envelope script's run and the envelope it wrote: the
    process, and the parsed payload (None when the freeze refused, so no
    envelope was written)."""

    process: subprocess.CompletedProcess[str]
    payload: dict | None


def run_freeze(
    script: Path, reports: list[dict]
) -> FreezeRun:
    """Run the freeze/envelope `script` over `reports`; return the process
    and the envelope payload it wrote (`--out`), parsed, or None when the
    freeze refused (non-zero exit, so nothing was written).

    The one freeze harness the four suites share: N reports written as
    `run{i}.json` in a temp dir, the script invoked with `--out` and their
    paths, the payload read back on success. The child environment is the
    ambient one — a freeze script reads no comparator knob.
    """
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        paths = []
        for index, body in enumerate(reports):
            path = root / f"run{index}.json"
            path.write_text(json.dumps(body))
            paths.append(path)
        out = root / "out.json"
        cmd = [sys.executable, str(script), "--out", str(out), *map(str, paths)]
        result = subprocess.run(
            cmd, text=True, capture_output=True, check=False
        )
        payload = json.loads(out.read_text()) if result.returncode == 0 else None
    return FreezeRun(result, payload)



def scrubbed_env(extra_env: dict[str, str] | None = None) -> dict[str, str]:
    """The child environment for comparator subprocess tests.

    `GITHUB_ACTIONS` and every comparator override prefix in
    SCRUB_PREFIXES are scrubbed, so the comparators' documented defaults
    apply no matter what the host shell (or a GitHub Actions runner,
    which exports `GITHUB_ACTIONS=true` ambiently) carries; a test opts
    variables back in through `extra_env`. The copy-scrub-override shape
    is child_env's (the one policy); this call site only widens the scrub
    vocabulary from exact names to the knob prefixes.
    """
    scrub = [
        "GITHUB_ACTIONS",
        *(key for key in os.environ if key.startswith(SCRUB_PREFIXES)),
    ]
    return child_env(scrub, extra_env)


def run_comparator(
    script: Path,
    baseline: dict,
    current: dict,
    pair_argv: Callable[[Path, Path], list[str]],
    flags: tuple[str, ...] = (),
    *,
    extra_env: dict[str, str] | None = None,
    sidecars: dict[str, str] | None = None,
) -> tuple[subprocess.CompletedProcess[str], dict[str, str | None]]:
    """The one pair-write + invoke primitive: materialize `baseline` and
    `current` as JSON reports in a temp dir and run `script` over them as
    `[python, script, *pair_argv(base_path, current_path), *flags]`.

    `pair_argv` is the comparator's own spelling of the report pair — the
    positional `BASE CURRENT` of compare-loadtest.py, the
    `--baseline B --report R` of compare-query-latency.py — so one
    primitive serves both CLIs and no caller re-rolls the temp dir and its
    two report files.

    `sidecars` maps a comparator flag to the file name it writes inside
    the temp dir (compare-loadtest.py's `--markdown-out` / `--status-out`
    are one binding's, never the primitive's); the returned map is keyed
    by that flag, holding the file's contents or None when the comparator
    never wrote it — empty for a comparator without sidecars. The child
    environment is `scrubbed_env`, so the comparator's documented defaults
    apply no matter what the developer's shell exports; a test re-adds
    either through `extra_env`.
    """
    env = scrubbed_env(extra_env)
    with tempfile.TemporaryDirectory() as tmp:
        base_path = Path(tmp) / "base.json"
        current_path = Path(tmp) / "current.json"
        base_path.write_text(json.dumps(baseline))
        current_path.write_text(json.dumps(current))
        cmd = [sys.executable, str(script), *pair_argv(base_path, current_path), *flags]
        sidecar_paths = {}
        for flag, name in (sidecars or {}).items():
            path = Path(tmp) / name
            sidecar_paths[flag] = path
            cmd += [flag, str(path)]
        result = subprocess.run(
            cmd, text=True, capture_output=True, check=False, env=env
        )
        written = {
            flag: path.read_text() if path.exists() else None
            for flag, path in sidecar_paths.items()
        }
    return result, written


def run_pair(
    baseline: dict,
    current: dict,
    flags: tuple[str, ...] = (),
    *,
    sidecars: tuple[str, ...] = (),
    extra_env: dict[str, str] | None = None,
) -> tuple[subprocess.CompletedProcess[str], dict[str, str | None]]:
    """Run `scripts/compare-loadtest.py` on a report pair; return
    (process, sidecar contents keyed by flag).

    The compare-loadtest.py binding of `run_comparator`: it states the
    comparator's positional pair spelling and resolves the `sidecars`
    subset through PAIR_SIDECARS (compare-query-latency.py takes neither),
    while the temp dir, the report pair, the child environment and the run
    stay the primitive's. A flag outside PAIR_SIDECARS is a KeyError, never
    a silently unwritten file.

    The returned map holds one entry per requested flag, its file's
    contents or None when the comparator never wrote it.
    """
    return run_comparator(
        COMPARE,
        baseline,
        current,
        lambda base, cur: [str(base), str(cur)],  # compare-loadtest: BASE CURRENT
        flags,
        extra_env=extra_env,
        sidecars={flag: PAIR_SIDECARS[flag] for flag in sidecars},
    )


def render_mode(
    baseline: dict,
    current: dict,
    flags: tuple[str, ...] = (),
    *,
    sidecars: tuple[str, ...] = (),
    extra_env: dict[str, str] | None = None,
) -> tuple[subprocess.CompletedProcess[str], dict[str, str | None]]:
    """Run the comparator in render mode; return (process, sidecar contents
    keyed by flag).

    The render-mode invocation both comparator suites share: `--render-only`
    with the requested `sidecars`, through the same scrubbed environment as
    `run_pair`. Extra `flags` (e.g. `--report-kind`) are appended after
    `--render-only`. `render` is this call's markdown binding.
    """
    return run_pair(
        baseline,
        current,
        ("--render-only", *flags),
        sidecars=sidecars,
        extra_env=extra_env,
    )


def render(
    baseline: dict,
    current: dict,
    flags: tuple[str, ...] = (),
    *,
    extra_env: dict[str, str] | None = None,
) -> tuple[subprocess.CompletedProcess[str], str]:
    """Run the comparator in render mode; return (process, markdown table).

    The render shorthand both comparator suites previously duplicated:
    `--render-only` plus a `--markdown-out` sidecar, with the table read
    back and asserted non-None. Extra `flags` (e.g. `--report-kind`) are
    appended after `--render-only`; the child environment is scrubbed
    exactly as in `run_pair`, and `extra_env` opts variables back in.
    """
    result, written = render_mode(
        baseline, current, flags, sidecars=("--markdown-out",), extra_env=extra_env
    )
    table = written["--markdown-out"]
    assert table is not None
    return result, table


class MemoryCompare(NamedTuple):
    """One memory-comparator run and the artefacts it wrote: the process,
    the rendered document (`--doc`), and the parsed verdict (`--verdict`)."""

    process: subprocess.CompletedProcess[str]
    document: str
    verdict: dict


def frozen_envelope(result: FreezeRun) -> dict:
    """The frozen envelope `result` wrote, its write asserted: `run_freeze`
    answers None only when the freeze refused, which is not what a case
    comparing against the envelope is testing — so the Optional dies here,
    once, instead of at every call site.
    """
    assert result.process.returncode == 0, result.process.stderr
    assert result.payload is not None, "the freeze wrote no envelope"
    return result.payload


def run_memory_comparator(
    script: Path,
    baseline: dict,
    current: dict,
    *,
    extra_env: dict[str, str] | None = None,
) -> MemoryCompare:
    """Run the memory comparator `script` over a report pair; return the
    process and its two artefacts, parsed.

    The memory comparators' binding of `run_comparator`: they spell the
    pair `--baseline`/`--report` (not compare-loadtest.py's positional
    pair) and write their document and verdict through MEMORY_SIDECARS'
    flags, so the temp dir, the report pair, the child environment (the
    scrub) and the run stay the primitive's — never a per-suite copy that
    passes `os.environ` straight through.
    """
    result, written = run_comparator(
        script,
        baseline,
        current,
        lambda base, cur: ["--baseline", str(base), "--report", str(cur)],
        extra_env=extra_env,
        sidecars=dict(MEMORY_SIDECARS),
    )
    document = written["--doc"]
    verdict = written["--verdict"]
    assert document is not None, "the comparator wrote no --doc document"
    assert verdict is not None, "the comparator wrote no --verdict payload"
    return MemoryCompare(result, document, json.loads(verdict))
