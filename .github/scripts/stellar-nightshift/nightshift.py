#!/usr/bin/env python3
"""Night-shift autopilot planner (LABS-311, ADR-0292).

Turns carried-open stellar findings (ledgers) into a durable queue of
small finding-clusters and decides whether the autopilot may work now.
Pure stdlib; no network — the workflow feeds it ledgers and the list of
files under human-driven work, and consumes the queue it prints.

Commands:
    cluster       Print one queue cluster (or one field) for the worker.
    check-scope   Fail when a branch's changed files (stdin) leave the cluster.
    window    Exit 0 when now is inside the CET window, 1 otherwise.
    plan      Cluster open findings into a queue (JSON).

    nightshift.py window [--now ISO] [--start 3] [--end 7]
    nightshift.py plan --ledger L [--ledger L ...] [--busy-files F]
                       [--now ISO] [--escalate-at 25] [--max-files 8]
                       [--out queue.json] [--force]

Rules enforced here, not in prose:
  * window: 03:00–07:00 Europe/Berlin (CET/CEST handled by zoneinfo);
    outside it `plan` emits no runnable work unless --force.
  * one cluster = one crate directory (one branch/PR); a cluster is split
    so it never spans more than --max-files files (never a monster PR).
  * clusters touching a busy file (open human PR / in-flight rebase) are
    held back, not dropped.
  * escalation: >= --escalate-at open findings accumulated → `escalate`
    is true and the window gate no longer defers *planning* (work still
    waits for the window; escalation only raises priority in the report).
  * test tier per cluster: `unit` when it stays inside one crate, `full`
    when it spans crates or touches a shared crate (core/model/error).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
from datetime import datetime, timezone
from zoneinfo import ZoneInfo

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

TZ = ZoneInfo("Europe/Berlin")
SEV_RANK = {"blocker": 0, "medium": 1, "nit": 2}
SHARED_CRATES = {"relate-core", "relate-model", "relate-error"}
# Ledger text rides PR comments (attacker-influenceable): only paths of this
# shape ever reach a shell command or an edit scope. The BODY (unanchored)
# is the one source of the shape: linear_queue compiles its in-text
# mention pattern from it, so the vocabulary can never drift between the
# acceptance gate and the scanner (LABS-684 round 3).
SAFE_PATH_BODY = r"(?:relate|surfaces|testing)/[A-Za-z0-9_.\-]+/[A-Za-z0-9_./\-]+"
SAFE_PATH_RE = re.compile(r"^" + SAFE_PATH_BODY + r"$")
SAFE_PKG_RE = re.compile(r"^[A-Za-z0-9_\-]+$")
CRATE_RE = re.compile(r"^((?:relate|surfaces|testing)/[^/]+)/")


def in_window(now: datetime, start: int = 3, end: int = 7) -> bool:
    return start <= now.astimezone(TZ).hour < end


def parse_now(raw: str | None) -> datetime:
    if not raw:
        return datetime.now(timezone.utc)
    dt = datetime.fromisoformat(raw)
    return dt if dt.tzinfo else dt.replace(tzinfo=timezone.utc)


def crate_of(path: str) -> str:
    m = CRATE_RE.match(path)
    return m.group(1) if m else (path.split("/", 1)[0] or ".")


def safe_path(p: str) -> bool:
    return bool(SAFE_PATH_RE.match(p)) and ".." not in p.split("/")


def open_findings(ledgers: list[dict]) -> list[dict]:
    seen: dict[tuple, dict] = {}
    for led in ledgers:
        for f in led.get("findings", {}).values():
            if f.get("status") != "open" or not safe_path(f.get("path", "")):
                continue
            # the same finding carried in two ledgers is one queue item
            seen.setdefault((f["path"], f.get("checklist_id", ""), f.get("quote_hash", f["id"])), f)
    return sorted(seen.values(), key=lambda f: (SEV_RANK.get(f["severity"], 3), f["path"], f["id"]))


def cluster(findings: list[dict], max_files: int) -> list[dict]:
    by_crate: dict[str, list[dict]] = {}
    for f in findings:
        by_crate.setdefault(crate_of(f["path"]), []).append(f)
    out: list[dict] = []
    for crate, fs in sorted(by_crate.items()):
        chunk: list[dict] = []
        files: set[str] = set()
        for f in fs:
            if f["path"] not in files and len(files) >= max_files:
                out.append(_cluster(crate, chunk))
                chunk, files = [], set()
            chunk.append(f)
            files.add(f["path"])
        if chunk:
            out.append(_cluster(crate, chunk))
    out.sort(key=lambda c: (c["top_severity_rank"], -len(c["findings"]), c["id"]))
    for i, c in enumerate(out, 1):
        c["id"] = f"NS-{i:03d}-{c['crate'].rsplit('/', 1)[-1]}"
    return out


def _cluster(crate: str, fs: list[dict]) -> dict:
    files = sorted({f["path"] for f in fs})
    crates = {crate_of(p) for p in files}
    shared = any(c.rsplit("/", 1)[-1] in SHARED_CRATES for c in crates)
    packages = sorted({c.rsplit("/", 1)[-1] for c in crates if SAFE_PKG_RE.match(c.rsplit("/", 1)[-1])})
    tier = "full" if shared or len(crates) > 1 else "unit"
    ident = "|".join(sorted(f"{f['path']}:{f.get('checklist_id', '')}:{f.get('quote_hash', f['id'])}" for f in fs))
    return {
        "id": crate,
        "key": hashlib.sha1(ident.encode()).hexdigest()[:10],
        "crate": crate,
        "packages": packages,
        "test_commands": test_commands(tier, packages),
        "trusted": False,
        "files": files,
        "findings": [{"id": f["id"], "severity": f["severity"], "path": f["path"],
                      "summary": f.get("summary", ""), "remedy": f.get("remedy", "")} for f in fs],
        "top_severity_rank": min(SEV_RANK.get(f["severity"], 3) for f in fs),
        "test_tier": tier,
    }


def test_commands(tier: str, packages: list[str]) -> list[str]:
    """Fast tier first (LABS-310): `unit` runs only the touched package(s);
    `full` runs the workspace, for changes in shared or multiple crates."""
    if not packages:
        tier = "full"
    pk = " ".join(f"-p {p}" for p in packages) or "--workspace"
    cmds = ["cargo fmt --all --check", f"cargo clippy {pk} --all-targets -- -D warnings"]
    cmds.append(f"cargo test {pk}" if tier == "unit"
                else "cargo test --workspace --exclude relate-python")
    return cmds


def out_of_scope(cluster: dict, changed: list[str]) -> list[str]:
    """Files an autopilot branch may not touch: anything outside the
    cluster's crate dirs (plus its own docs/tests inside them)."""
    allowed = {crate_of(p) for p in cluster["files"]}
    return sorted(p for p in changed if crate_of(p) not in allowed)


def plan(ledgers: list[dict], busy: set[str], now: datetime, escalate_at: int,
         max_files: int, force: bool, tasks: list[dict] | None = None) -> dict:
    findings = open_findings(ledgers)
    clusters = cluster(findings, max_files)
    # hand-filed review-tasks issues with a Files: scope (linear_queue.py
    # pull): an input stream — each issue's files are already shaped into
    # per-crate clusters upstream (task_clusters); never 1:1 execution
    # KeyError on a stale-contract task document is deliberate: a
    # pre-rename tasks.json (singular `cluster`) must fail loudly, not
    # silently contribute zero work (the no-silent-fallback rule)
    clusters += [c for t in (tasks or []) for c in t["clusters"]]
    runnable, held = [], []
    for c in clusters:
        (held if busy & set(c["files"]) else runnable).append(c)
    window = in_window(now)
    go = window or force
    return {
        "version": 1,
        "generated_at": now.astimezone(TZ).isoformat(),
        "in_window": window,
        "open_findings": len(findings),
        "escalate": len(findings) >= escalate_at,
        "runnable": runnable if go else [],
        "deferred_outside_window": [] if go else runnable,
        "held_busy": [{"id": c["id"], "files": sorted(busy & set(c["files"]))} for c in held],
    }


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    w = sub.add_parser("window")
    w.add_argument("--now")
    w.add_argument("--start", type=int, default=3)
    w.add_argument("--end", type=int, default=7)
    g = sub.add_parser("cluster", help="print one queue cluster (id or key) as JSON")
    g.add_argument("--queue", required=True)
    g.add_argument("--id", required=True)
    g.add_argument("--field", help="print one field (lists one per line)")
    c = sub.add_parser("check-scope", help="exit 1 if changed files (stdin) leave the cluster's crates")
    c.add_argument("--queue", required=True)
    c.add_argument("--id", required=True)
    p = sub.add_parser("plan")
    p.add_argument("--ledger", action="append", default=[])
    p.add_argument("--busy-files", help="file with newline-separated paths under human work")
    p.add_argument("--tasks", help="tasks.json from `linear_queue.py pull`")
    p.add_argument("--now")
    p.add_argument("--escalate-at", type=int, default=25)
    p.add_argument("--max-files", type=int, default=8)
    p.add_argument("--out")
    p.add_argument("--force", action="store_true")
    args = ap.parse_args()

    if args.cmd == "window":
        sys.exit(0 if in_window(parse_now(args.now), args.start, args.end) else 1)

    if args.cmd in ("cluster", "check-scope"):
        with open(args.queue) as fh:
            q = json.load(fh)
        cl = next((c for c in q["runnable"] if args.id in (c["id"], c["key"])), None)
        if cl is None:
            sys.exit(f"nightshift: no runnable cluster {args.id}")
        if args.cmd == "cluster":
            v = cl[args.field] if args.field else cl
            print("\n".join(v) if isinstance(v, list) else (v if isinstance(v, str) else json.dumps(v, indent=1)))
            return
        bad = out_of_scope(cl, [ln.strip() for ln in sys.stdin if ln.strip()])
        for b in bad:
            print(f"out of scope: {b}", file=sys.stderr)
        sys.exit(1 if bad else 0)

    ledgers = []
    for path in args.ledger:
        with open(path) as fh:
            ledgers.append(json.load(fh))
    busy: set[str] = set()
    if args.busy_files:
        with open(args.busy_files) as fh:
            busy = {ln.strip() for ln in fh if ln.strip()}
    tasks = None
    if args.tasks:
        with open(args.tasks) as fh:
            tasks = json.load(fh)
    if not ledgers and tasks is None:
        sys.exit("nightshift: need --ledger and/or --tasks")
    q = plan(ledgers, busy, parse_now(args.now), args.escalate_at, args.max_files, args.force, tasks)
    text = json.dumps(q, indent=1, sort_keys=True) + "\n"
    if args.out:
        # The plugin's tools carry an ensure-parent helper (fileout.py);
        # this worker must stay runnable from a pristine checkout with
        # no plugin install (the fix job's verifier, ADR-0292), so the
        # two-line idiom is spelled inline here.
        out = os.path.abspath(args.out)
        os.makedirs(os.path.dirname(out), exist_ok=True)
        with open(out, "w") as fh:
            fh.write(text)
    else:
        sys.stdout.write(text)
    print(f"nightshift: {q['open_findings']} open, {len(q['runnable'])} runnable, "
          f"{len(q['held_busy'])} held, in_window={q['in_window']}, escalate={q['escalate']}",
          file=sys.stderr)


if __name__ == "__main__":
    main()
