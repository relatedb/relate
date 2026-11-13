"""LABS-1545 throwaway: PROFILE each pokec query once through the CLI (auto engine) and print, per query, the
frontier decline reasons, the kernel rungs and the LABS1545 cost-trace lines (stderr). Summary lines only."""
import re
import subprocess
import sys
import time

cli, store, qdir, *qids = sys.argv[1:]
for q in qids:
    text = open(f"{qdir}/q{int(q):02d}.cypher").read()
    t = time.time()
    try:
        p = subprocess.run([cli, "query", "--mode", "json", "--read-only", "-d", store, "PROFILE " + text],
                           capture_output=True, text=True, timeout=300,
                           env={"PATH": "/usr/bin:/bin", "RELATE_TELEMETRY": "off", "RELATE_MAX_ROWS": "0",
                                "LABS1545_TRACE": "1", "RELATE_PATTERN_ENGINE": "auto"})
    except subprocess.TimeoutExpired:
        print(f"q{q}: PROFILE timeout 300s")
        continue
    out = p.stdout
    decl = sorted(set(re.findall(r'"frontier_decline"\s*:\s*"([^"]*)"', out)))
    rungs = sorted(set(re.findall(r'"rung"\s*:\s*"([^"]*)"', out)))
    cost = [l.strip() for l in p.stderr.splitlines() if l.startswith("LABS1545")]
    print(f"q{q}: exit={p.returncode} {time.time() - t:.1f}s rungs={rungs} frontier_decline={decl or 'none'}")
    for c in cost:
        print("    " + c)
    if p.returncode != 0:
        print("    stderr: " + (p.stderr.strip().splitlines() or [''])[-1][:200])
