#!/usr/bin/env python3
"""Before/after wall + peak RSS + answer hash of pokec queries (throwaway CI scratch)."""
import hashlib, json, os, shutil, signal, subprocess, sys, time
from pathlib import Path

clis = {"before": sys.argv[1], "after": sys.argv[2]}
master, qdir, out = Path(sys.argv[3]), Path(sys.argv[4]), Path(sys.argv[5])
timeout = float(os.environ.get("RCA_TIMEOUT", "330"))
plan = json.loads(os.environ["RCA_PLAN"])  # {"before": [qids], "after": [qids]}
out.mkdir(parents=True, exist_ok=True)
work = master.parent / "rca-work.rl"
env = dict(os.environ, RELATE_TELEMETRY="off", RELATE_MAX_ROWS="0")

def kb(pid, key):
    try:
        for ln in open(f"/proc/{pid}/status"):
            if ln.startswith(key):
                return int(ln.split()[1])
    except OSError:
        pass
    return None

def run(cli, script, tag, to):
    if work.is_dir():
        shutil.rmtree(work)
    elif work.exists():
        work.unlink()
    shutil.copytree(master, work) if master.is_dir() else shutil.copy(master, work)
    of = out / f"{tag}.out"
    t0 = time.time()
    with open(of, "wb") as fo, open(out / f"{tag}.err", "wb") as fe:
        p = subprocess.Popen([cli, "query", "--mode", "jsonl", "-d", str(work)], stdin=subprocess.PIPE,
                             stdout=fo, stderr=fe, env=env, start_new_session=True)
        p.stdin.write(script.encode()); p.stdin.close()
        hwm = 0; timed_out = False
        while p.poll() is None:
            time.sleep(0.25)
            h = kb(p.pid, "VmHWM:")
            if h: hwm = max(hwm, h)
            if time.time() - t0 > to:
                timed_out = True
                os.killpg(p.pid, signal.SIGKILL); p.wait(); break
        wall = time.time() - t0
    lines = sorted(open(of, "rb").read().splitlines())
    digest = hashlib.sha256(b"\n".join(lines)).hexdigest()[:16]
    shutil.rmtree(work, ignore_errors=True) if work.is_dir() else work.unlink(missing_ok=True)
    return dict(wall_s=round(wall, 2), timeout=timed_out, rc=p.returncode, hwm_mib=hwm // 1024,
                rows=len(lines), sha=digest)

res = {}
for side in ("before", "after"):
    res[f"{side}/baseline"] = run(clis[side], "RETURN 1 AS x;\n", f"{side}-baseline", 600)
    print(side, "baseline", res[f"{side}/baseline"], flush=True)
    base = res[f"{side}/baseline"]["wall_s"]
    for n in plan[side]:
        q = (qdir / f"q{n:02d}.cypher").read_text()
        r = run(clis[side], q, f"{side}-q{n}", timeout)
        r["query_s_est"] = round(r["wall_s"] - base, 1)
        res[f"{side}/q{n}"] = r
        print(side, f"q{n}", r, flush=True)
(out / "results.json").write_text(json.dumps(res, indent=1))
print("\n== summary ==")
qs = sorted({k.split("/")[1] for k in res if "q" in k.split("/")[1][:1] and k.split("/")[1] != "baseline"}, key=lambda s: int(s[1:]))
for q in qs:
    b, a = res.get(f"before/{q}"), res.get(f"after/{q}")
    fmt = lambda r: "-" if not r else ("TIMEOUT>%ss" % int(r["query_s_est"]) if r["timeout"] else "%ss/%dMiB" % (r["query_s_est"], r["hwm_mib"] - res["after/baseline"]["hwm_mib"]))
    same = "-" if not (a and b) else ("same" if a["sha"] == b["sha"] and a["rows"] == b["rows"] else "DIFF")
    print(q, "before", fmt(b), "after", fmt(a), "answers", same, "rows", b and b["rows"], a and a["rows"])
