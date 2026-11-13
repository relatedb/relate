"""LABS-1545 throwaway: median ms per pokec query per mode from the per-run pytest-benchmark JSONs."""
import glob
import json
import os
import re
import sys

d = sys.argv[1]
rows = {}
for f in sorted(glob.glob(f"{d}/q*-*.json")):
    m = re.match(r"q(\d+)-(.+)\.json", os.path.basename(f))
    try:
        doc = json.load(open(f))
        stats = [b["stats"]["median"] * 1000 for b in doc.get("benchmarks", [])]
    except Exception:  # noqa: BLE001
        continue
    if stats:
        rows.setdefault(int(m[1]), {})[m[2]] = stats[0]
print("| query | auto ms | walk ms | nfa ms | auto2 ms | auto/walk | nfa/walk | best |")
print("|---|---:|---:|---:|---:|---:|---:|---|")
for q, r in sorted(rows.items()):
    g = lambda k: r.get(k)  # noqa: E731
    cells = [f"{g(k):.2f}" if g(k) is not None else "-" for k in ("auto", "walk", "nfa", "auto2")]
    aw = f"{g('auto')/g('walk'):.2f}" if g("auto") and g("walk") else "-"
    nw = f"{g('nfa')/g('walk'):.2f}" if g("nfa") and g("walk") else "-"
    pool = {k: v for k, v in r.items() if k != "auto2"}
    print(f"| q{q} | {' | '.join(cells)} | {aw} | {nw} | {min(pool, key=pool.get) if pool else '-'} |")
