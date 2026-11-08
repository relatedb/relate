"""Shared loadtest fixtures for the scripts/tests/ suites.

The comparator suites (`test_compare_loadtest.py`,
`test_compare_baselines.py`, `test_compare_verdict.py`) and the
freeze-envelope suite (`test_freeze_baseline_envelope.py`) validate report
shapes that the baseline workflow versions (ADR-0071 / ADR-0289). They
previously carried verbatim per-file copies of these builders, so a schema
bump had to edit every copy in lockstep; a missed copy left one suite
silently validating a stale report shape. The builders now live here,
exactly once.

Also here, because it is the other fact those suites share: the
`aggregator_env` contract — the child environment `scripts/compare-baselines.sh`
reads when a suite drives it under test.

Imported as a plain sibling module: the Makefile runs each test file
directly with `python3`, so `sys.path[0]` is `scripts/tests/`.
"""

import sys
from collections.abc import Mapping
from pathlib import Path

from child_env import REPORTING_ENV, child_env


def aggregator_env(
    baselines: Path,
    reports: Path,
    overrides: Mapping[str, str | None] | None = None,
) -> dict[str, str]:
    """The child environment for `scripts/compare-baselines.sh` under test.

    The aggregator reads its inputs from `BASELINES`/`REPORTS_DIR` and its
    interpreter from `PYTHON`, and it appends to `$GITHUB_STEP_SUMMARY`
    whenever the runner set one. The two suites that drive it —
    `test_compare_baselines.py` as its subject, `test_update_pr_comment.py`
    for the comparator → aggregator → updater round trip — state that
    contract here, once, on top of the one scrub policy (child_env's
    `REPORTING_ENV`). `overrides` applies last, so a case adds a name or
    removes one (a `None` value removes it, child_env's convention).
    """
    return child_env(
        REPORTING_ENV,
        {
            "BASELINES": str(baselines),
            "REPORTS_DIR": str(reports),
            "PYTHON": sys.executable,
            **(overrides or {}),
        },
    )


ENV = {
    "class": "relate-loadtest-baseline/v1",
    "arch": "x86_64",
    "os": "linux",
    "build_image": "rust:1.88-bookworm@sha256:aaa",
    "run_image": "debian:bookworm-slim@sha256:bbb",
    "cpus": 2,
    "cpu_period_us": 10000,
    "cpu_quota_us": 20000,
    "memory": "2g",
    "ops": 20000,
    "warmup": 8,
    "workers": 8,
    "runner": "github-actions:ubuntu-24.04",
}


def report(
    *,
    p99: int,
    errors: int = 0,
    schema: int | None = 6,
    kind: str | None = "fraud",
    env: dict | None = ENV,
    ops_per_sec: float = 100.0,
    logical_used_bytes: int = 1000,
    usecases: list[dict] | None = None,
    ops: list[int] | None = None,
    p50: int | None = None,
) -> dict:
    """A generic loadtest report; every suite validates this shape.

    `ops` (raw per-op samples) and `p50` are the freeze-envelope suite's
    extras: a report carries a bulky `ops` list that the envelope freeze
    must strip (`ops_omitted`), and a second latency stat that the
    worst-of-N merge must widen alongside p99. Both default to absent, so
    the comparator suites' fixtures are unchanged.
    """
    latency_ns = {"p99": p99}
    if p50 is not None:
        latency_ns["p50"] = p50
    out = {
        "throughput": {"ops_per_sec": ops_per_sec},
        "memory": {"logical_used_bytes": logical_used_bytes},
        "usecases": usecases
        or [
            {
                "id": "query",
                "ops": 100,
                "errors": errors,
                "latency_ns": latency_ns,
            }
        ],
    }
    if ops is not None:
        out["ops"] = ops
    if schema is not None:
        out["schema_version"] = schema
    if kind is not None:
        out["workload"] = {"kind": kind}
    if env is not None:
        out["environment"] = env
    return out


def memory_report(*, logical: int, ratio: float, env: dict | None = ENV) -> dict:
    return {
        "schema_version": 1,
        "workload": {"kind": "memory-shape"},
        "environment": env,
        "shapes": [
            {
                "shape": "baseline",
                "index": "none",
                "logical_bytes": 0,
                "ratio": None,
            },
            {
                "shape": "deep",
                "index": "none",
                "logical_bytes": logical,
                "ratio": ratio,
            },
        ],
    }


def checkpoint_report(*, logical: int = 7_211_831, env: dict | None = ENV) -> dict:
    return {
        "name": "fraud-checkpoint",
        "logical_used_bytes": logical,
        "rss_bytes": 84_414_464,
        "data_file_bytes": 678_934,
        "environment": env,
    }
