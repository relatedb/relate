"""LABS-1545 throwaway: the sqlite-gate rows, auto vs nfa vs walk, interleaved in one process.

Runs inside the sqg side environment (PYTHONPATH=<benchmarks>/benchmarks/sqlite-gate/harness). Three in-memory
databases per graph are opened with RELATE_PATTERN_ENGINE=auto|nfa|walk set at open (the engine reads it once, at
Database::open), loaded identically, then every round times the row on all three, in rotating order. Writes a JSON
file and prints a summary table only (medians, ratios, rung, frontier decline, load average).
"""
from __future__ import annotations

import json
import os
import statistics
import sys
import time

from sqg import engines, gen, parity, workloads

ROWS = ["khop3-349", "khop4-349", "khop5-349", "khop3-876",
        "cochange-d1-530", "cochange-d2-530", "cochange-d3-530", "cochange-d4-530"]
MODES = ["auto", "nfa", "walk"]
ROUNDS = int(os.environ.get("LABS1545_ROUNDS", "7"))


def load1() -> float:
    return os.getloadavg()[0]


def main(out_path: str) -> None:
    lane = engines.RelateLane()
    cases = {c.id: c for c in workloads.all_cases()}
    graphs: dict[int, tuple] = {}
    dbs: dict[tuple[int, str], object] = {}
    result = {"rounds": ROUNDS, "cpus": os.cpu_count(), "load_start": load1(), "rows": {}}
    for rid in ROWS:
        case = cases[rid]
        if case.size not in graphs:
            g = gen.generate(case.size)
            graphs[case.size] = (g, lane.prepare(g))
            for mode in MODES:
                os.environ["RELATE_PATTERN_ENGINE"] = mode
                conn = lane.open()
                lane.load(conn, graphs[case.size][1])
                dbs[(case.size, mode)] = conn
            os.environ.pop("RELATE_PATTERN_ENGINE", None)
        step = case.steps[0]
        row = {"load_start": load1(), "ms": {m: [] for m in MODES}}
        digests = {}
        for m in MODES:                       # the first call per mode: parity + warm
            digests[m] = parity.digest_rows([parity.normalize(lane.run(dbs[(case.size, m)], step))])
        row["parity_ok"] = len(set(digests.values())) == 1
        for r in range(ROUNDS):
            order = MODES[r % 3:] + MODES[:r % 3]
            for m in order:
                t = time.perf_counter()
                lane.run(dbs[(case.size, m)], step)
                row["ms"][m].append((time.perf_counter() - t) * 1000)
        row["load_end"] = load1()
        row["rung"] = lane.rung(dbs[(case.size, "auto")], step)
        for m in ("nfa", "walk"):
            row[f"rung_{m}"] = lane.rung(dbs[(case.size, m)], step)
        # the throwaway cost trace (LABS1545_TRACE): one auto execution, the engine prints one line to stderr
        os.environ["LABS1545_TRACE"] = "1"
        sys.stderr.write(f"LABS1545 row={rid}\n"); sys.stderr.flush()
        lane.run(dbs[(case.size, "auto")], step)
        os.environ.pop("LABS1545_TRACE")
        result["rows"][rid] = row
    result["load_end"] = load1()
    json.dump(result, open(out_path, "w"), indent=1)
    print(f"cpus={result['cpus']} loadavg start={result['load_start']:.2f} end={result['load_end']:.2f} rounds={ROUNDS}")
    print("| row | auto ms | nfa ms | walk ms | auto/walk | nfa/walk | best | auto rung | decline | parity |")
    print("|---|---:|---:|---:|---:|---:|---|---|---|---|")
    for rid, row in result["rows"].items():
        med = {m: statistics.median(v) for m, v in row["ms"].items()}
        ops = (row["rung"] or {}).get("operators") or [{}]
        best = min(med, key=med.get)
        print(f"| {rid} | {med['auto']:.2f} | {med['nfa']:.2f} | {med['walk']:.2f} | {med['auto']/med['walk']:.2f} | "
              f"{med['nfa']/med['walk']:.2f} | {best} | {','.join((row['rung'] or {}).get('rungs', []))} | "
              f"{ops[0].get('decline')} | {row['parity_ok']} |")


if __name__ == "__main__":
    main(sys.argv[1])
