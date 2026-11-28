#!/usr/bin/env python3
"""Compare a query-latency report against the frozen envelope (ADR-0245).

Hard failures, independent of environment class: any query not ok
(wrong answer, error, timeout — the answer gate is ADR-0243's), a missing
qid/profile or required field (status, engine stats, peak RSS) in the
report, or a report schema the envelope does not know.

A db_hits band runs in every class (the count is deterministic, so it
needs no matching environment): per (qid, index profile), report db_hits <=
max(DB_HITS_RATIO x envelope, envelope + DB_HITS_FLOOR); an envelope entry
without db_hits is skipped until the next freeze.

Latency and memory bands run only when the `environment` classes match
(ADR-0071): per (qid, index profile), engine p50 <= max(P50_RATIO x
envelope p50, envelope p50 + P50_FLOOR_MS); across the run, peak RSS <=
RSS_RATIO x envelope. A class mismatch is INCOMPATIBLE: hard fail in
GitHub Actions (CI_COMPARE=1), local `make gate` skips the perf bands and
still enforces the hard failures.

The 0.5 ms floor (ADR-0245) is chosen so a 10x regression of a
sub-millisecond query fails: 0.17 ms -> 1.7 ms is a 10x jump that a
millisecond-scale allowance would swallow.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import sys
from pathlib import Path

# The ADR-0071 environment-class model, exactly once (the shared module
# the loadtest compares import too): the canonical field-level mismatch
# walk decides the class gate.
_ENV = Path(__file__).resolve().parent / "loadtest_env.py"
_env_spec = importlib.util.spec_from_file_location("loadtest_env", _ENV)
_env = importlib.util.module_from_spec(_env_spec)
_env_spec.loader.exec_module(_env)  # type: ignore[union-attr]

# The p50 band factor: measured p50 must stay within this multiple of the
# envelope's worst-of-N p50 (plus the absolute floor below). 3.0, not the
# ADR-0245 text's 1.5: the envelope is frozen by the freeze workflow on a
# quiet host, while the per-PR compare runs inside the full CI matrix,
# where concurrent jobs contend for the same physical host's cores and a
# CFS-quota'd 2-vCPU container sees less real CPU (the maiden gated run
# measured up to 2.5x on the CPU-bound queries — the ADR-0245 later
# reference records the widening). The ratchet's purpose is unaffected:
# the catastrophic plan-change class it exists for (q3: 0.03ms -> 78ms)
# is thousands of times over any of these bands.
P50_RATIO = float(os.environ.get("QUERY_LATENCY_P50_RATIO", "3.0"))
P50_FLOOR_MS = float(os.environ.get("QUERY_LATENCY_P50_FLOOR_MS", "0.5"))
RSS_RATIO = float(os.environ.get("QUERY_LATENCY_RSS_RATIO", "1.15"))
# The store-read ratchet: db_hits is deterministic, so this band is tight
# (5%) and carries a small absolute floor so a tiny query does not flap on
# a one-read plan change. A decrease always passes.
DB_HITS_RATIO = float(os.environ.get("QUERY_LATENCY_DB_HITS_RATIO", "1.05"))
DB_HITS_FLOOR = int(os.environ.get("QUERY_LATENCY_DB_HITS_FLOOR", "64"))


def load(path: Path) -> dict:
    return json.loads(path.read_text())


def hard_failures(baseline: dict, report: dict, rep: dict, base: dict) -> list[str]:
    """The unconditional layer: answers, coverage, required fields on
    both sides, both schemas. Judged in every environment class — a
    malformed report or envelope fails loudly, never passes on defaults
    (the no-silent-fallbacks rule)."""
    diags: list[str] = []
    for key, q in rep.items():
        if q.get("status") is None:
            diags.append(f"q{key[0]}/{key[1]} missing status")
        elif q["status"] != "ok":
            diags.append(
                f"q{key[0]}/{key[1]} not ok: {q['status']} {q.get('detail') or ''}"
            )
        if "p50_ns" not in (q.get("engine") or {}):
            diags.append(f"q{key[0]}/{key[1]} missing engine.p50_ns")
        if not isinstance(q.get("db_hits"), int):
            diags.append(f"q{key[0]}/{key[1]} missing db_hits")
    if report.get("peak_rss_bytes") is None:
        diags.append("report missing peak_rss_bytes")
    for key in sorted(base):
        if key not in rep:
            diags.append(f"q{key[0]}/{key[1]} missing from the report")
        if base[key].get("p50_ms") is None:
            diags.append(f"q{key[0]}/{key[1]} envelope entry missing p50_ms")
    if baseline.get("schema_version") != 1:
        diags.append(f"unknown envelope schema_version {baseline.get('schema_version')}")
    # The report's own schema (report.rs SCHEMA, bumped when a field
    # changes meaning or disappears): an envelope is meaningless across
    # a report-schema bump, in both directions.
    if report.get("schema") != 1:
        diags.append(f"unknown report schema {report.get('schema')}")
    return diags


def bands(base: dict, rep: dict) -> list[str]:
    """The class-gated layer: per-query p50 bands (ADR-0245). Runs only
    on a clean, class-matched pair — the callers gate it."""
    diags: list[str] = []
    for key in sorted(base):
        q = rep.get(key)
        if q is None:
            continue
        p50 = q["engine"]["p50_ns"] / 1e6
        env = base[key]["p50_ms"]
        allowed = max(P50_RATIO * env, env + P50_FLOOR_MS)
        if p50 > allowed:
            diags.append(
                f"q{key[0]}/{key[1]} p50 {p50:.3f} ms > {allowed:.3f} ms "
                f"(envelope {env:.3f} ms; ADR-0245 bands)"
            )
    return diags


def db_hits_bands(base: dict, rep: dict) -> list[str]:
    """The store-read ratchet: per query, report db_hits <= max(DB_HITS_RATIO
    x envelope, envelope + DB_HITS_FLOOR). db_hits is deterministic for a
    fixed graph and plan, so this layer is not class-gated: it judges every
    environment. An envelope entry frozen before the field existed carries
    no db_hits and is skipped until the next freeze."""
    diags: list[str] = []
    for key in sorted(base):
        env = base[key].get("db_hits")
        if env is None:
            continue
        # hard_failures already refused a report missing any envelope key,
        # so the lookup is total here; a broken call order raises.
        q = rep[key]
        allowed = max(DB_HITS_RATIO * env, env + DB_HITS_FLOOR)
        if q["db_hits"] > allowed:
            diags.append(
                f"q{key[0]}/{key[1]} db_hits {q['db_hits']} > {allowed:.0f} "
                f"(envelope {env}; ADR-0245 store-read band)"
            )
    return diags


def verdict(baseline: dict, report: dict) -> tuple[str, list[str]]:
    """The one public verdict layer: (status, diagnostics). Builds the
    keyed views, then runs the layers in order: the hard-failure layer
    (unconditional — a hard failure wins every status, answers are judged
    in every class), the store-read band (db_hits is deterministic, so it
    is class-independent and also unconditional), the class gate, and
    last the p50 and memory bands on a clean, matched pair. Status is
    `ok`, `fail`, or `incompatible` (class mismatch: the p50 and memory
    bands skipped, hard failures and the store-read band already
    judged)."""
    rep: dict[tuple, dict] = {}
    for q in report.get("queries") or []:
        rep[(q.get("qid"), q.get("index_profile"))] = q
    base: dict[tuple, dict] = {
        (q.get("qid"), q.get("index_profile")): q for q in baseline.get("queries") or []
    }
    diags = hard_failures(baseline, report, rep, base)
    if diags:
        return "fail", diags
    diags = db_hits_bands(base, rep)
    if diags:
        return "fail", diags
    # Class gate (ADR-0071), through the canonical field-level walk
    # (loadtest_env.environment_mismatches): a mismatch skips the bands
    # (INCOMPATIBLE in Actions) but never softens a hard failure above.
    if _env.environment_mismatches(baseline.get("environment"), report.get("environment")):
        return "incompatible", []
    diags = bands(base, rep)
    env_rss = baseline.get("peak_rss_bytes")
    rss = report.get("peak_rss_bytes")
    if env_rss is not None and rss is not None and rss > RSS_RATIO * env_rss:
        diags.append(f"peak RSS {rss} > {RSS_RATIO}x {env_rss}")
    if diags:
        return "fail", diags
    return "ok", []


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--baseline", required=True, type=Path)
    ap.add_argument("--report", required=True, type=Path)
    args = ap.parse_args()

    status, diags = verdict(load(args.baseline), load(args.report))
    for d in diags:
        print(f"compare-query-latency: {d}", file=sys.stderr)
    if status == "fail":
        return 1
    if status == "incompatible":
        print(
            "compare-query-latency: environment class mismatch (ADR-0071): "
            "latency and memory bands skipped; answers still judged.",
            file=sys.stderr,
        )
        return 1 if _env.in_github_actions() else 0
    print("compare-query-latency: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
