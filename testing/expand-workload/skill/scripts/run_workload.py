#!/usr/bin/env python3
"""Validate a query workload against a dataset (references/validation.md).

Reads a JSONL workload, executes every query, classifies the outcome, captures
rows and median timing, guards the dataset against writes, and writes a JSON
report. Exit 1 on protocol violations (leaked writes, unclassified queries,
unreasoned skips); engine errors and timeouts are findings, not failures.

    python3 run_workload.py queries.jsonl --engine relate --store db.rl --out report.json
    python3 run_workload.py queries.jsonl --engine relate --store :memory: --setup setup.cypher
    python3 run_workload.py queries.jsonl --engine neo4j   # NEO4J_URI/USERNAME/PASSWORD env
"""

import argparse
import collections
import json
import re
import shutil
import statistics
import sys
import tempfile
import time
from pathlib import Path
from typing import NamedTuple, Optional


class _Section(NamedTuple):
    """The section context a block inherits from its headings: kind (`==`),
    tier (`===`), area (`====`)."""

    kind: str
    tier: Optional[str]
    area: Optional[str]


class _Heading(NamedTuple):
    """A section heading — its level (2 kind, 3 tier, 4 area) and title."""

    level: int
    title: str


class _Block(NamedTuple):
    """A cypher block — its attribute string and its query text."""

    attrs: str
    query: str

TIMEOUT_S = 10.0  # soft budget per execution; overruns are reported as timeouts


# ── engines ──────────────────────────────────────────────────────────────────


def _count_nodes_rels(db):
    """Node/relationship counts on one connection — the one place this pair exists."""
    nodes = db.execute("MATCH (n) RETURN count(n) AS c").fetchall()[0]["c"]
    rels = db.execute("MATCH ()-[r]->() RETURN count(r) AS c").fetchall()[0]["c"]
    return {"nodes": nodes, "rels": rels}


class _RelateBase:
    """Shared scaffolding: the untouched original path, the setup text, and
    the working store (a temp-dir clone for file stores, so writes never
    touch the original)."""

    def __init__(self, store, setup=None):
        self._original = store  # the untouched store, for the leak check
        self._setup_text = Path(setup).read_text() if setup else None
        self.store = (
            store
            if store == ":memory:"
            else _clone_store(store, tempfile.mkdtemp(prefix="workload-clone-"))
        )

    def _setup_statements(self):
        return _split_statements(self._setup_text) if self._setup_text else []

    def counts(self):
        """None (not {}) so the report can say the leak check is disabled
        instead of folding a sentinel into "the store was empty"."""
        return None

    def original_counts(self):
        return None

    def close(self):
        pass


class RelateWheelEngine(_RelateBase):
    """relate via the Python wheel.

    Three connections, always distinct: the read db (read-only for a file
    store, so the write connection can take the writer lease), the write db
    (a second connection to the clone, or a fresh :memory: rebuild of the
    setup), and the read-only probe of the original store for the leak check.
    """

    def __init__(self, wheel, store, setup=None):
        super().__init__(store, setup)
        self._wheel = wheel
        # :memory: stays writable — its writes go to a separate db anyway
        self.db = wheel.connect(self.store, read_only=self.store != ":memory:")
        self._write_db = None
        if self._setup_text:
            if self.store == ":memory:":
                self._apply_setup(self.db)
            else:
                # the read db is read-only, so setup runs on a throwaway
                # writable connection to the clone
                setup_db = wheel.connect(self.store)
                try:
                    self._apply_setup(setup_db)
                finally:
                    setup_db.close()

    def _apply_setup(self, db):
        for stmt in self._setup_statements():
            db.execute(stmt, {})

    def execute(self, query, params):
        return len(self.db.execute(query, params).fetchall())

    def execute_rolled_back(self, query, params):
        """Writes never touch the read db: a *second* connection to the clone
        (file store) or a fresh :memory: rebuild of the setup absorbs them."""
        if self._write_db is None:
            self._write_db = self._wheel.connect(self.store)
            if self.store == ":memory:":  # a fresh memory db needs the dataset
                self._apply_setup(self._write_db)
        return len(self._write_db.execute(query, params).fetchall())

    def counts(self):
        return _count_nodes_rels(self.db)

    def original_counts(self):
        """Counts on the *original* store, opened read-only — the leak check."""
        if self._original == ":memory:":
            return self.counts()
        probe = self._wheel.connect(self._original, read_only=True)
        try:
            return _count_nodes_rels(probe)
        finally:
            probe.close()

    def close(self):
        self.db.close()
        if self._write_db is not None:
            self._write_db.close()


class RelateCliEngine(_RelateBase):
    """relate via the CLI, one statement per invocation.

    No counting — each invocation is a fresh process (:memory: carries
    nothing between them), so the leak check reports disabled.
    """

    def __init__(self, store, setup=None):
        super().__init__(store, setup)
        for stmt in self._setup_statements():
            _run_cli(self.store, stmt, {})

    def execute(self, query, params):
        return _run_cli(self.store, query, params)

    execute_rolled_back = execute  # the CLI store is already the clone


def relate_engine(store, setup=None):
    """The wheel when the package is importable, else the CLI per statement."""
    try:
        import relate
    except ImportError:
        relate = None
    if relate is not None:
        return RelateWheelEngine(relate, store, setup)
    if shutil.which("relate") is None:
        raise SystemExit(
            "relate engine needs the relate Python package or the relate CLI"
        )
    return RelateCliEngine(store, setup)


class Neo4jEngine:
    def __init__(self):
        try:
            from neo4j import GraphDatabase
        except ImportError:
            raise SystemExit("neo4j engine needs the neo4j Python package")
        import os

        uri = os.environ.get("NEO4J_URI", "neo4j://localhost:7687")
        auth = (os.environ.get("NEO4J_USERNAME", "neo4j"), os.environ.get("NEO4J_PASSWORD", ""))
        self._driver = GraphDatabase.driver(uri, auth=auth)
        self._driver.verify_connectivity()

    def execute(self, query, params):
        with self._driver.session() as s:
            return len(s.run(query, params).data())

    def execute_rolled_back(self, query, params):
        with self._driver.session() as s:
            tx = s.begin_transaction()
            try:
                n = len(tx.run(query, params).data())
            finally:
                tx.rollback()
            return n

    def counts(self):
        with self._driver.session() as s:
            nodes = s.run("MATCH (n) RETURN count(n) AS c").single()["c"]
            rels = s.run("MATCH ()-[r]->() RETURN count(r) AS c").single()["c"]
        return {"nodes": nodes, "rels": rels}

    def original_counts(self):
        """Writes always rolled back — the original counts are the live counts."""
        return self.counts()

    def close(self):
        self._driver.close()


def _clone_store(src, dst_dir):
    """Copy the store into a temp dir; writes happen on the copy."""
    src = Path(src)
    dst = Path(dst_dir) / src.name
    if src.is_dir():
        shutil.copytree(src, dst)
    else:
        shutil.copy2(src, dst)
    return str(dst)


def _split_statements(text):
    """Split on top-level semicolons — never inside a string literal or a
    comment. Mirrors the rule relate-splitter owns for the Rust side; the
    wheel exposes no split API, so the skill carries this small mirror."""
    out, buf, quote = [], [], None
    i = 0
    while i < len(text):
        ch = text[i]
        nxt = text[i + 1] if i + 1 < len(text) else ""
        if quote:
            buf.append(ch)
            if ch == quote:
                quote = None
        elif ch in ("'", '"', "`"):
            quote = ch
            buf.append(ch)
        elif ch == "/" and nxt == "/":
            i = text.find("\n", i)
            if i == -1:
                break
            buf.append("\n")
        elif ch == "/" and nxt == "*":
            end = text.find("*/", i + 2)
            i = len(text) if end == -1 else end + 1
            # one separator for either comment shape, so tokens on either
            # side of a comment can never merge (RETURN 1/*x*/AS one)
            buf.append(" ")
        elif ch == ";":
            out.append("".join(buf))
            buf = []
        else:
            buf.append(ch)
        i += 1
    if buf:
        out.append("".join(buf))
    return [s.strip() for s in out if s.strip()]


# The adoc cypher-block matcher is scanner-only: a quote-aware walk of
# the header, plus one tail regex for the fences and the query. The
# header language (attr values may be quoted with '...' or "...", and
# quoted values may contain `]`) has no single-regex spelling that is
# both backtracking-free (the ambiguous alternation was the ReDoS this
# rewrite cleared) and safe on the 3.9 floor (the possessive quantifier
# that would disambiguate it is Python 3.11+ only, and failed the module
# import on 3.9/3.10) — so the header is not regexed at all:
# `_header_closer` tracks the active quote and returns the first `]`
# outside quotes in one O(n) pass, and the tail regex takes the fence
# pair and the query past that closer.
_BLOCK_OPEN = "[source,cypher"
# The one source for the block tail — the fence pair around the query —
# so no second spelling of the tail can drift.
_TAIL_SRC = r"\s*\n----\s*\n(.*?)\n----\s*$"
_ADOC_TAIL = re.compile(f"(?ms){_TAIL_SRC}")


def _block_starts(text):
    """Index of every line-start block open (position 0 or right after a
    newline), located at C speed so long documents never walk engine
    positions."""
    starts = []
    pos = text.find(_BLOCK_OPEN)
    while pos != -1:
        if pos == 0 or text[pos - 1] == "\n":
            starts.append(pos)
        pos = text.find(_BLOCK_OPEN, pos + 1)
    return starts


def _header_closer(text, start):
    """Index of the `]` that closes the header attrs beginning at `start`
    — the first `]` outside quotes — or None when there is none: either
    no `]` exists past `start`, or a quote opened and never closes, so
    every `]` past it is attribute text."""
    quote = None
    i = start
    while True:
        if quote is not None:
            close = text.find(quote, i)
            if close == -1:
                return None
            quote = None
            i = close + 1
            continue
        # outside quotes, the earliest of ] ' " decides — each located at
        # C speed, so long attr spans never walk character by character
        found = (text.find("]", i), text.find("'", i), text.find('"', i))
        nxt = min((p for p in found if p != -1), default=-1)
        if nxt == -1:
            return None  # neither a quote nor a closer remains
        if text[nxt] == "]":
            return nxt
        quote, i = text[nxt], nxt + 1


def _adoc_blocks(text):
    """(position, _Block) per cypher block, in document order: the
    scanner finds each candidate's header closer, the tail regex takes
    the fence pair and the query past it. A block already taken is never
    entered again, so blocks never overlap."""
    blocks = []
    consumed = 0
    for start in _block_starts(text):
        if start < consumed:
            continue  # inside a block already taken
        attrs_start = start + len(_BLOCK_OPEN)
        closer = _header_closer(text, attrs_start)
        if closer is None:
            continue  # no header closer — not a block
        tail = _ADOC_TAIL.match(text, closer + 1)
        if tail is None:
            continue  # no fence pair past the closer — no block
        blocks.append((start, _Block(text[attrs_start:closer], tail.group(1))))
        consumed = tail.end()
    return blocks


def load_workload(path: Path):
    """Entries from a JSONL workload or an executable .adoc guide.

    Adoc blocks carry expectations as attributes:
    `[source,cypher,expect="error:Detail",expect_ms="500",slow,skip="reason"]`;
    the enclosing `== Kind === Tier ==== Area` headers give kind/tier/area.
    """
    if path.suffix != ".adoc":
        return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]

    text = path.read_text()
    entries = []
    kind = "read"
    tier = None
    area = None
    # headers and blocks in document order, so each block inherits the
    # enclosing section's kind/tier/area (two passes merged by position:
    # one alternation with DOTALL lets the header branch eat whole blocks)
    tokens = sorted(
        [
            (m.start(), _Heading(len(m.group(1)), m.group(2).strip()))
            for m in re.finditer(r"(?m)^(=+) (.+)$", text)
        ]
        + _adoc_blocks(text),
        key=lambda t: t[0],
    )
    for _, event in tokens:
        if isinstance(event, _Heading):
            # dispatch on the heading level — == kind, === tier,
            # ==== area — the title only picks the value within its level,
            # so a narrative `== Round 2 — paging` header cannot clobber area
            level, title = event.level, event.title
            low = title.lower()
            if level == 2:
                for k in ("read", "write"):
                    if low.startswith(k):
                        kind = k
            elif level == 3:
                for t in ("simple", "mid", "complex"):
                    if low.startswith(t):
                        tier = t
            elif level == 4:
                area = re.sub(r"\s*\(\d+\)\s*$", "", title)
            continue
        entries.append(
            _entry_from_block(
                event.attrs,
                event.query,
                _Section(kind, tier, area),
                fallback_id=f"adoc.{len(entries):04d}",
                source_name=path.name,
            )
        )
    return entries


def _entry_from_block(attrs, query, section, *, fallback_id, source_name):
    """One workload entry from a cypher block's attribute string.

    Owns every attribute handler — the accretion point for new attributes.
    Malformed values skip the entry with the reason recorded, never run
    unparameterized or die on a bare traceback.
    """
    attr_map = _parse_attrs(attrs)
    entry = {
        "id": attr_map.get("id", fallback_id),
        # block attributes win over the enclosing section — sections are narrative
        "area": attr_map.get("area", section.area),
        "tier": attr_map.get("tier", section.tier),
        "kind": attr_map.get("kind", section.kind),
        "query": query,
        "params": {},
        "source": f"adoc:{source_name}",
    }
    if "expect" in attr_map:
        value = attr_map["expect"]
        if isinstance(value, str):
            expect, _, detail = value.partition(":")
        else:
            # a bare `expect` parses to True — treat it like an unknown value
            expect, detail = value, None
        # the closed set verdict() models — a hand-edit typo skips with a
        # reason instead of silently degrading to ok-semantics
        if expect in ("ok", "error"):
            entry["expect"] = expect
            if detail:
                entry["expect_detail"] = detail
        else:
            _note_skip(entry, f"unknown expect value {expect!r} (want ok|error)")
    if "slow" in attr_map:
        entry["expect_slow"] = True
    if "skip" in attr_map:
        _note_skip(entry, attr_map["skip"])
    if "extends" in attr_map:
        entry["extends"] = attr_map["extends"]
    try:
        if "expect_ms" in attr_map:
            entry["expect_ms"] = float(attr_map["expect_ms"])
        if "round" in attr_map:
            entry["round"] = int(attr_map["round"])
    except (TypeError, ValueError) as exc:
        _note_skip(entry, f"unparsable numeric attr ({exc})")
    if "params" in attr_map:
        try:
            entry["params"] = json.loads(attr_map["params"])
        except (TypeError, json.JSONDecodeError) as exc:
            _note_skip(entry, f"unparsable params attr ({exc})")
    return entry


def _note_skip(entry, reason):
    """Skip reasons accumulate — co-occurring causes are all recorded."""
    # a bare `skip` attribute parses to True — coerce to a stated reason
    if not isinstance(reason, str):
        reason = "skipped (no reason given)"
    prior = entry.get("skip_reason")
    entry["skip_reason"] = f"{prior}; {reason}" if prior else reason


def _parse_attrs(attrs):
    """Block attributes into a dict — split on commas outside quoted values."""
    out = {}
    buf, quote = [], None
    parts = []
    for ch in attrs:
        if quote:
            buf.append(ch)
            if ch == quote:
                quote = None
        elif ch in ("'", '"'):
            quote = ch
            buf.append(ch)
        elif ch == ",":
            parts.append("".join(buf))
            buf = []
        else:
            buf.append(ch)
    parts.append("".join(buf))
    for part in parts:
        part = part.strip()
        if "=" in part:
            k, v = part.split("=", 1)
            out[k.strip()] = _unquote(v.strip())
        elif part:
            out[part] = True
    return out


def _unquote(v):
    """Remove one matching pair of surrounding quotes — and only a matching
    pair, so a value legitimately ending in a quote keeps it."""
    if len(v) >= 2 and v[0] == v[-1] and v[0] in ("'", '"'):
        return v[1:-1]
    return v


def _run_cli(store, query, params):
    """relate CLI fallback: one statement per invocation."""
    import subprocess

    cmd = ["relate", "--json", "-c", query, store]
    for k, v in params.items():
        cmd += ["--param", f"{k}={json.dumps(v)}"]
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=TIMEOUT_S * 2)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"exit {proc.returncode}")
    if not proc.stdout.strip():
        raise RuntimeError("relate CLI exited 0 with empty stdout — nothing to count")
    return len(json.loads(proc.stdout))


# ── validation loop ─────────────────────────────────────────────────────────


def classify(fn, timeout_s):
    """Run fn with a soft timeout. Returns (status, rows, ms, full error text)."""
    start = time.perf_counter()
    try:
        rows = fn()
        ms = (time.perf_counter() - start) * 1000
        return ("timeout" if ms > timeout_s * 1000 else "ok"), rows, ms, None
    except Exception as e:  # engine rejection is a finding
        ms = (time.perf_counter() - start) * 1000
        # full text — the expectation match must see all of it; the report
        # truncates for storage, after matching
        return "engine-error", None, ms, f"{type(e).__name__}: {e}"


def verdict(entry, status, err):
    """Map (entry, raw status) to the reported status.

    Expectations annotated at generation time (expect/expect_detail,
    expect_slow) decide what a result *means*, not just what happened.
    Returns (status, err, slow) — slow only for a tolerated expect_slow
    timeout; expect_ms budgets are judged by the caller, which owns the
    median this function cannot see.
    """
    expect = entry.get("expect", "ok")
    if expect == "error":
        if status == "ok":
            return "unexpected-ok", "expected an error but the query ran", False
        if status == "engine-error":
            detail = entry.get("expect_detail")
            if detail and detail not in (err or ""):
                return "wrong-error", f"{err} (expected detail {detail!r})", False
            return "expected-error", err, False
        return status, err, False
    if status == "timeout" and entry.get("expect_slow"):
        return "ok", err, True  # tolerated timeout stays marked slow
    return status, err, False


def _record(q, status, rows=None, median_ms=None, error=None, repeated=None, slow=None):
    """The one result-record shape, shared by executed and skipped entries."""
    return {
        "id": q.get("id"),
        "area": q.get("area"),
        "tier": q.get("tier"),
        "kind": q["kind"],
        "round": q.get("round", 0),
        "extends": q.get("extends"),
        "status": status,
        "rows": rows,
        "median_ms": median_ms,
        "error": error,
        "repeated": repeated,
        "slow": slow,
    }


def run(workload, engine, repeat, timeout_s):
    for q in workload:
        q.setdefault("kind", "read")  # one defaulting rule, applied once
    skipped = [
        _record(q, "skipped", error=q["skip_reason"]) for q in workload if q.get("skip_reason")
    ]
    executable = [q for q in workload if not q.get("skip_reason")]
    reads = [q for q in executable if q["kind"] == "read"]
    writes = [q for q in executable if q["kind"] == "write"]
    results = []
    for i, entry in enumerate(reads + writes):  # reads first; writes last, guarded
        if i % 100 == 0:
            print(f"  ... {i}/{len(reads) + len(writes)}", file=sys.stderr)
        kind = entry["kind"]
        query, params = entry["query"], entry.get("params") or {}
        runs = []
        attempts = max(1, repeat) if kind == "read" else 1
        run_query = engine.execute_rolled_back if kind == "write" else engine.execute
        for _ in range(attempts):
            status, rows, ms, err = classify(lambda: run_query(query, params), timeout_s)
            runs.append((status, rows, ms, err))
            if status != "ok":
                break  # no repeats for failures
        status, rows, _, err = runs[0]
        timings = [ms for (st, _, ms, _) in runs if st == "ok"]
        median = statistics.median(timings) if timings else None

        status, err, slow = verdict(entry, status, err)
        budget_ms = entry.get("expect_ms")
        if not slow and status == "ok" and budget_ms and median is not None:
            slow = median > budget_ms  # only over-budget medians count
        if err is not None and len(err) > 300:
            err = err[:300] + "…"  # truncate for storage, after matching
        # writes are idempotent probes, not benchmarks — one run, never repeated
        results.append(
            _record(
                entry,
                status,
                rows,
                round(median, 3) if median is not None else None,
                err,
                repeated=kind == "read" and attempts > 1,
                slow=slow or None,
            )
        )
    return results + skipped


def _p95(ts):
    """Nearest-rank p95 of an ascending list — the convention this report
    has always used (the max when fewer than 20 samples)."""
    return ts[min(len(ts) - 1, -(-len(ts) * 95 // 100) - 1)]


def _defects(results):
    # engine accepts what must fail (unexpected-ok) or raises the wrong
    # error (wrong-error) are the sharpest findings an expectation carries
    return [
        {"id": r["id"], "status": r["status"], "error": r["error"]}
        for r in results
        if r["status"] in ("engine-error", "timeout", "unexpected-ok", "wrong-error")
    ]


def _expectations(results):
    return {
        "matched_errors": sum(1 for r in results if r["status"] == "expected-error"),
        "unexpected_ok": sum(1 for r in results if r["status"] == "unexpected-ok"),
        "wrong_error": sum(1 for r in results if r["status"] == "wrong-error"),
        "slow": sum(1 for r in results if r.get("slow")),
    }


def _rounds(results):
    """Status counts per round."""
    rounds = collections.defaultdict(collections.Counter)
    for r in results:
        rounds[str(r["round"])][r["status"]] += 1
    return {rnd: dict(counts) for rnd, counts in rounds.items()}


def _perf(results):
    ok = [r for r in results if r["median_ms"] is not None]
    per_round_ms = collections.defaultdict(list)
    for r in ok:
        per_round_ms[str(r["round"])].append(r["median_ms"])
    # median sorts internally — no pre-sort
    per_round = {rnd: round(statistics.median(ts), 3) for rnd, ts in per_round_ms.items()}
    per_tier = {}
    for tier in ("simple", "mid", "complex"):
        ts = sorted(r["median_ms"] for r in ok if r["tier"] == tier)
        if ts:
            per_tier[tier] = {
                "median_ms": round(statistics.median(ts), 3),
                "p95_ms": round(_p95(ts), 3),
                "max_ms": round(ts[-1], 3),
            }
    return {
        "per_tier": per_tier,
        "slowest": sorted(
            ({"id": r["id"], "median_ms": r["median_ms"]} for r in ok),
            key=lambda x: -x["median_ms"],
        )[:10],
        "per_round": per_round,
    }


def _diff_previous(results, prev):
    """Regressions and new-in-latest-round ids vs the previous report."""
    if not prev:
        return [], []
    prev_status = {q["id"]: q["status"] for q in prev.get("queries", [])}
    latest_round = max((r.get("round", 0) for r in results), default=0)
    regressions = [
        {"id": r["id"], "round": r.get("round", 0), "was": was, "now": r["status"]}
        for r in results
        if (was := prev_status.get(r["id"])) and was != r["status"]
    ]
    new_this_round = [
        r["id"]
        for r in results
        if r.get("round", 0) == latest_round and r["id"] not in prev_status
    ]
    return regressions, new_this_round


def build_report(engine_name, store, counts_before, counts_after, results, prev=None):
    by_status = collections.Counter(r["status"] for r in results)
    coverage = collections.defaultdict(collections.Counter)
    for r in results:
        coverage[r["area"] or "?"][r["tier"] or "?"] += 1
    rounds = _rounds(results)
    regressions, new_this_round = _diff_previous(results, prev)
    return {
        "engine": engine_name,
        "store": store,
        "dataset_counts": counts_before,
        "dataset_counts_after": counts_after,
        # "disabled" when the engine backend cannot count (CLI fallback) —
        # said in the open instead of a silent {}-as-empty-store
        "leak_check": "checked" if counts_before is not None and counts_after is not None else "disabled",
        "summary": {"total": len(results), **by_status},
        "coverage": coverage,
        "rounds": rounds,
        "regressions": regressions,
        "new_this_round": new_this_round,
        "expectations": _expectations(results),
        "defects": _defects(results),
        "perf": _perf(results),
        "queries": results,
    }


def protocol_violations(report, counts_before, counts_after, workload):
    problems = []
    if counts_before is not None and counts_after is not None and counts_before != counts_after:
        problems.append(
            f"writes leaked to the store: counts {counts_before} -> {counts_after}"
        )
    valid = ("ok", "engine-error", "timeout", "skipped", "expected-error", "unexpected-ok", "wrong-error")
    for r in report["queries"]:
        if r["status"] not in valid:
            problems.append(f"{r['id']}: unclassified status {r['status']!r}")
    reasons = {q.get("id"): q.get("skip_reason") for q in workload if q.get("skip_reason")}
    for r in report["queries"]:
        if r["status"] == "skipped" and not reasons.get(r["id"]):
            problems.append(f"{r['id']}: skipped without a recorded reason")
    return problems


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("workload", type=Path, help="queries.jsonl")
    ap.add_argument("--engine", choices=["relate", "neo4j"], required=True)
    ap.add_argument("--store", help="relate store path (or :memory:)")
    ap.add_argument("--setup", type=Path, help="statements to build an :memory: dataset")
    ap.add_argument("--repeat", type=int, default=3, help="timing runs per read query")
    ap.add_argument("--timeout", type=float, default=TIMEOUT_S, help="soft per-query budget (s)")
    ap.add_argument("--out", type=Path, default=Path("report.json"))
    ap.add_argument(
        "--prev",
        type=Path,
        help="the previous round's report.json — the report diffs against it (regressions)",
    )
    args = ap.parse_args()

    workload = load_workload(args.workload)
    if not workload:
        raise SystemExit("empty workload")

    if args.engine == "relate":
        if not args.store:
            raise SystemExit("--store is required for the relate engine")
        engine = relate_engine(args.store, args.setup)
    else:
        engine = Neo4jEngine()

    counts_before = counts_after = None
    try:
        counts_before = engine.counts()
        results = run(workload, engine, args.repeat, args.timeout)
        # both engines implement the full protocol — no getattr sniffing
        counts_after = engine.original_counts()
    finally:
        engine.close()

    prev = json.loads(args.prev.read_text()) if args.prev else None
    report = build_report(args.engine, args.store, counts_before, counts_after, results, prev)
    args.out.write_text(json.dumps(report, indent=2) + "\n")

    s = report["summary"]
    print(
        f"{s.get('ok', 0)}/{s['total']} ok; "
        f"{s.get('engine-error', 0)} engine-error, "
        f"{s.get('timeout', 0)} timeout, {s.get('skipped', 0)} skipped -> {args.out}"
    )
    for d in report["defects"][:10]:
        print(f"  {d['status']}: {d['id']}: {d['error']}")
    if report["regressions"]:
        print("REGRESSIONS (status changed vs --prev):")
        for g in report["regressions"][:10]:
            print(f"  {g['id']} (round {g['round']}): {g['was']} -> {g['now']}")

    problems = protocol_violations(report, counts_before, counts_after, workload)
    if problems:
        for p in problems:
            print(f"PROTOCOL VIOLATION: {p}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
