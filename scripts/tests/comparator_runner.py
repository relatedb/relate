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

Imported as a plain sibling module: the Makefile runs each test file
directly with `python3`, so `sys.path[0]` is `scripts/tests/`.
"""

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

from child_env import child_env


ROOT = Path(__file__).resolve().parents[2]
COMPARE = ROOT / "scripts" / "compare-loadtest.py"

# Every env-overridable knob vocabulary the served comparators read,
# spelled once: the loadtest bands (LOADTEST_*) and the query-latency
# ADR-0245 bands (QUERY_LATENCY_*). A future comparator knob extends
# this tuple — never a second scrub branch.
SCRUB_PREFIXES = ("LOADTEST_", "QUERY_LATENCY_")


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


def run_pair(
    baseline: dict,
    current: dict,
    flags: tuple[str, ...] = (),
    *,
    markdown: bool = False,
    status: bool = False,
    extra_env: dict[str, str] | None = None,
) -> tuple[subprocess.CompletedProcess[str], str | None, str | None]:
    """Run the comparator CLI on a report pair; return (process, table,
    status token).

    `table`/`token` are the --markdown-out/--status-out file contents when
    the corresponding kwarg is set (None when the flag was absent or the
    file was never written).

    The child environment is `scrubbed_env` above, so the comparator's
    documented defaults apply no matter what the developer's shell
    exports; a test re-adds either through `extra_env`.
    """
    env = scrubbed_env(extra_env)
    with tempfile.TemporaryDirectory() as tmp:
        base_path = Path(tmp) / "base.json"
        current_path = Path(tmp) / "current.json"
        base_path.write_text(json.dumps(baseline))
        current_path.write_text(json.dumps(current))
        cmd = [sys.executable, str(COMPARE), str(base_path), str(current_path), *flags]
        table_path = status_path = None
        if markdown:
            table_path = Path(tmp) / "table.md"
            cmd += ["--markdown-out", str(table_path)]
        if status:
            status_path = Path(tmp) / "status.txt"
            cmd += ["--status-out", str(status_path)]
        result = subprocess.run(
            cmd, text=True, capture_output=True, check=False, env=env
        )
        table = (
            table_path.read_text() if table_path is not None and table_path.exists() else None
        )
        token = (
            status_path.read_text() if status_path is not None and status_path.exists() else None
        )
    return result, table, token


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
    result, table, _ = run_pair(
        baseline,
        current,
        ("--render-only", *flags),
        markdown=True,
        extra_env=extra_env,
    )
    assert table is not None
    return result, table
