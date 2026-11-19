#!/usr/bin/env python3
"""Model-economics measurement (LABS-311 item 4, ADR-0292).

    passstats.py yield --ledger L [...]     findings per pass area × severity
    passstats.py missrate --baseline A.json --candidate B.json

`missrate` answers "is the cheaper model good enough for this pass": run
the same briefs with the strong model (baseline) and the cheap model
(candidate), each producing a findings.json; the miss rate is the share of
baseline medium+ findings with no candidate finding on the same path whose
line range overlaps (±5 lines). Acceptable: <= 0.15 on >= 20 baseline
findings (below that the sample is too small to call).
"""

from __future__ import annotations

import argparse
import json
from collections import Counter

MIN_SAMPLE = 20
MAX_MISS = 0.15
SLACK = 5


def yield_table(ledgers: list[dict]) -> Counter:
    c: Counter = Counter()
    for led in ledgers:
        for f in led.get("findings", {}).values():
            c[(f.get("area", "?"), f["severity"])] += 1
    return c


def _hit(b: dict, cands: list[dict]) -> bool:
    b0, b1 = b["start_line"], b.get("end_line") or b["start_line"]
    return any(c["path"] == b["path"] and c["start_line"] - SLACK <= b1
               and (c.get("end_line") or c["start_line"]) + SLACK >= b0 for c in cands)


def miss_rate(baseline: list[dict], candidate: list[dict]) -> dict:
    base = [f for f in baseline if f["severity"] in ("blocker", "medium")]
    missed = [f for f in base if not _hit(f, candidate)]
    rate = len(missed) / len(base) if base else 0.0
    return {"baseline_findings": len(base), "missed": len(missed), "miss_rate": round(rate, 3),
            "verdict": ("insufficient-sample" if len(base) < MIN_SAMPLE
                        else "acceptable" if rate <= MAX_MISS else "too-lossy")}


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    y = sub.add_parser("yield")
    y.add_argument("--ledger", action="append", required=True)
    m = sub.add_parser("missrate")
    m.add_argument("--baseline", required=True)
    m.add_argument("--candidate", required=True)
    a = ap.parse_args()
    if a.cmd == "yield":
        for (area, sev), n in sorted(yield_table([json.load(open(p)) for p in a.ledger]).items()):
            print(f"{n:4d}  {area:12s} {sev}")
    else:
        print(json.dumps(miss_rate(json.load(open(a.baseline))["findings"],
                                   json.load(open(a.candidate))["findings"]), indent=1))


if __name__ == "__main__":
    main()
