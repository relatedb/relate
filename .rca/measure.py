#!/usr/bin/env python3
"""Per-query wall/peak-RSS/RSS-series measurement of the pokec q16-q34 family on one master store (RCA scratch)."""
import json, os, shutil, signal, subprocess, sys, time
from pathlib import Path

cli, master, qdir, out = sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3]), Path(sys.argv[4])
timeout = float(os.environ.get("RCA_TIMEOUT", "120"))
qids = [int(x) for x in os.environ.get("RCA_QIDS", ",".join(str(i) for i in range(16, 35))).split(",")]
out.mkdir(parents=True, exist_ok=True)
work = master.parent / "rca-work.rl"
env = dict(os.environ, RELATE_TELEMETRY="off", RELATE_MAX_ROWS="0")

def status_kb(pid, key):
    try:
        for ln in open(f"/proc/{pid}/status"):
            if ln.startswith(key):
                return int(ln.split()[1])
    except OSError:
        pass
    return None

def run(script, tag, to=timeout):
    if work.is_dir():
        shutil.rmtree(work)
    elif work.exists():
        work.unlink()
    shutil.copytree(master, work) if master.is_dir() else shutil.copy(master, work)
    db = work if master.is_dir() else work
    of = out / f"{tag}.out"
    t0 = time.time()
    with open(of, "wb") as fo, open(out / f"{tag}.err", "wb") as fe:
        p = subprocess.Popen([cli, "query", "--mode", "jsonl", "-d", str(db)], stdin=subprocess.PIPE, stdout=fo, stderr=fe,
                             env=env, start_new_session=True)
        p.stdin.write(script.encode()); p.stdin.close()
        hwm = 0; series = []; timed_out = False
        while p.poll() is None:
            time.sleep(0.25)
            h = status_kb(p.pid, "VmHWM:"); r = status_kb(p.pid, "VmRSS:")
            if h: hwm = max(hwm, h)
            if r and len(series) < 4000: series.append((round(time.time() - t0, 2), r // 1024))
            if time.time() - t0 > to:
                timed_out = True
                os.killpg(p.pid, signal.SIGKILL); p.wait(); break
        wall = time.time() - t0
    nout = sum(1 for _ in open(of, "rb"))
    shutil.rmtree(work, ignore_errors=True) if work.is_dir() else work.unlink(missing_ok=True)
    return dict(tag=tag, wall_s=round(wall, 2), timeout=timed_out, rc=p.returncode, hwm_mib=hwm // 1024, out_lines=nout, series=series[::max(1, len(series)//60)])

res = {}
res["baseline"] = run("RETURN 1 AS x;\n", "baseline", 600)
print("baseline", {k: v for k, v in res["baseline"].items() if k != "series"}, flush=True)
for n in qids:
    q = (qdir / f"q{n:02d}.cypher").read_text()
    r = run(".timer on\n" + q, f"q{n}")
    res[f"q{n}"] = r
    print(f"q{n}", {k: v for k, v in r.items() if k not in ("series",)}, flush=True)
    run("EXPLAIN " + q, f"q{n}-explain", 60)
    if r["wall_s"] < 60 and not r["timeout"]:
        pr = run("PROFILE " + q, f"q{n}-profile", 120)
        res[f"q{n}"]["profile_wall_s"] = pr["wall_s"]
(out / "results.json").write_text(json.dumps(res))
