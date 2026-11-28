#!/usr/bin/env python3
"""Freeze a worst-of-N latency envelope from repeated query-latency runs.

The query-latency suite (ADR-0243) reports, per query and index profile,
the engine-elapsed p50 and mean (ADR-0221), plus the run's peak RSS. A
single run's p50 is steady but not free of runner noise, so the envelope
(ADR-0245) keeps, per gated metric, the value that makes the gate hardest
to pass by accident and easiest for a healthy run to meet: the highest
p50, the highest mean, and the highest peak RSS across the runs. Load
time is recorded and never gated. `scripts/compare-query-latency.py`
applies the ADR-0245 bands on top of this envelope.

Reports must all carry the same `environment` class (ADR-0071 form,
recorded by the runner): a freeze from mixed classes is refused.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# The canonical ADR-0071 environment-class model (loadtest_env.py), the
# one module every environment comparison shares.
import importlib.util

_ENV = Path(__file__).resolve().parent / "loadtest_env.py"
_env_spec = importlib.util.spec_from_file_location("loadtest_env", _ENV)
_env = importlib.util.module_from_spec(_env_spec)
_env_spec.loader.exec_module(_env)  # type: ignore[union-attr]

SCHEMA = 1


def load(path: Path) -> dict:
    return json.loads(path.read_text())


def queries_key(rep: dict) -> tuple:
    """The report identity a freeze refuses to mix: the report schema
    (report.rs SCHEMA, bumped when a field changes meaning or
    disappears), suite, sf, index profile set, and the per-query qid
    set."""
    profiles = tuple(sorted({q.get("index_profile") or "" for q in rep.get("queries") or []}))
    return (
        rep.get("schema"),
        rep.get("suite"),
        rep.get("sf"),
        profiles,
        tuple(q.get("qid") for q in rep.get("queries") or []),
    )


def check_compatible(reports: list[tuple[Path, dict]]) -> list[str]:
    first_path, first = reports[0]
    problems: list[str] = []
    for path, rep in reports:
        # The canonical ADR-0071 field-level walk (loadtest_env), the one
        # model every environment comparison shares.
        for m in _env.environment_mismatches(first.get("environment"), rep.get("environment")):
            problems.append(f"{path}: {m.message}")
        if queries_key(rep) != queries_key(first):
            problems.append(f"{path}: query set differs from {first_path}")
        keys = [(q.get("qid"), q.get("index_profile")) for q in rep.get("queries") or []]
        if len(keys) != len(set(keys)):
            problems.append(f"{path}: duplicate (qid, index_profile) entry")
        # A report without db_hits is a pre-field run: refuse it, so an
        # envelope never silently freezes a zero store-read ceiling.
        for q in rep.get("queries") or []:
            if not isinstance(q.get("db_hits"), int):
                problems.append(f"{path}: q{q.get('qid')} report has no db_hits")
    return problems


def envelope(reports: list[tuple[Path, dict]]) -> dict:
    """The worst-of-N envelope: per (qid, index profile) the highest p50
    and highest mean of the engine elapsed; across the runs the highest
    peak RSS; the load times recorded, never gated."""
    first_path, first = reports[0]
    out: dict = {
        "schema_version": SCHEMA,
        "suite": first.get("suite"),
        "sf": first.get("sf"),
        "engine_version": first.get("engine_version"),
        "environment": first.get("environment"),
        # Load time is recorded and never gated; its home is the loaded
        # origin's LoadReport (`graph.load.seconds`), not the run config.
        "load_seconds": [
            (rep.get("graph") or {}).get("load", {}).get("seconds") for _, rep in reports
        ],
        "queries": [],
    }
    worst: dict[tuple, dict] = {}
    for _, rep in reports:
        for q in rep.get("queries") or []:
            engine = q.get("engine") or {}
            entry = worst.setdefault(
                (q.get("qid"), q.get("index_profile")),
                {
                    "qid": q.get("qid"),
                    "index_profile": q.get("index_profile"),
                    "p50_ms": 0.0,
                    "mean_ms": 0.0,
                    "db_hits": 0,
                    "rows": q.get("rows"),
                },
            )
            # Engine elapsed is non-negative: max over zeros and every
            # run's value keeps the worst of N.
            entry["p50_ms"] = max(entry["p50_ms"], engine.get("p50_ns", 0) / 1e6)
            entry["mean_ms"] = max(entry["mean_ms"], engine.get("mean_ns", 0) / 1e6)
            entry["db_hits"] = max(entry["db_hits"], q["db_hits"])
        rss = rep.get("peak_rss_bytes")
        if rss is not None:
            out["peak_rss_bytes"] = max(out.get("peak_rss_bytes") or 0, rss)
    out["queries"] = [worst[k] for k in sorted(worst, key=lambda k: (k[1] or "", k[0]))]
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--out", required=True, type=Path, help="envelope output path")
    ap.add_argument("reports", nargs="+", type=Path, help="the per-run report JSONs")
    args = ap.parse_args()

    reports: list[tuple[Path, dict]] = [(p, load(p)) for p in args.reports]
    problems = check_compatible(reports)
    if problems:
        for p in problems:
            print(f"freeze-query-latency: {p}", file=sys.stderr)
        return 1

    args.out.write_text(json.dumps(envelope(reports), indent=2) + "\n")
    print(f"freeze-query-latency: {args.out} ({len(reports)} runs)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
