#!/usr/bin/env python3
"""Finding-recurrence report (LABS-311, ADR-0292).

Counts findings by checklist id (the recurring-pattern key) across one or
more ledgers, so the drop after a prevention rule lands is measurable.

    recurrence.py --ledger L [--ledger L ...] [--since-round N] [--json]
"""

from __future__ import annotations

import argparse
import json
from collections import Counter


def count(ledgers: list[dict], since_round: int = 0) -> Counter:
    c: Counter = Counter()
    for led in ledgers:
        for f in led.get("findings", {}).values():
            if f.get("round_filed", 0) >= since_round:
                c[f.get("checklist_id") or "unclassified"] += 1
    return c


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--ledger", action="append", required=True)
    ap.add_argument("--since-round", type=int, default=0)
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()
    leds = [json.load(open(p)) for p in a.ledger]
    c = count(leds, a.since_round)
    if a.json:
        print(json.dumps(dict(c.most_common()), indent=1))
    else:
        for k, n in c.most_common():
            print(f"{n:4d}  {k}")


if __name__ == "__main__":
    main()
