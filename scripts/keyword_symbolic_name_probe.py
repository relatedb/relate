#!/usr/bin/env python3
"""Measure keyword spellings as unquoted symbolic names, Neo4j vs Relate.

Issue LABS-183, follow-up to ADR-0169: Neo4j accepts reserved words as
unquoted symbolic names (aliases after ``AS``, variable references, binding
variables, map keys); Relate rejects most of them. This probe measures
exactly which spellings Neo4j accepts in which positions, and what
Relate currently does with the same queries, so the follow-up ADR can
cite real evidence instead of assumptions.

The keyword list is derived from the one keyword-vocabulary declaration --
the ``declare_keywords!`` invocation in
``relate/relate-parser/src/token.rs``, from which ``lookup_keyword``'s
match cascade, the longest-spelling bound and the parser test's spelling
table are all generated (ADR-0505; 71 spellings, unchanged since ADR-0169)
-- there is no hand-copied list anywhere in this script. Every spelling is run through every shape in ``SHAPES``:
``alias`` (RETURN / WITH / UNWIND / LOAD CSV / CALL ... YIELD x AS),
``reference`` (a bare RETURN item, the first WITH item, an item after a
comma in a WITH list, an arithmetic operand, a function argument, an
aggregate argument, a WHERE operand, an ORDER BY key, a property-access
base, a subscript base, a list element, a comparison operand), ``binding``
(a node, relationship and path variable, FOREACH, list comprehension,
pattern comprehension, quantifier and reduce variables, a CALL (x) subquery
import) and ``key`` (a map literal key, a map projection key, a map
projection ``.prop`` selector, a map projection variable selector and a
pattern property-map key).

Fixture semantics
-----------------

``testdata/keyword-symbolic-names/probe.tsv`` -- one row per
``shape``/``keyword`` pair, TAB-separated, header ``id group shape keyword
neo4j relate query``. ``neo4j`` and ``relate`` are ``accepted``,
``syntax_error`` or ``other_error:<code or message>``. Neo4j outcomes come
from ``EXPLAIN`` against a live ``neo4j:2026.09-community`` container;
Relate outcomes come from ``EXPLAIN`` through
``target/debug/relate :memory:`` built from this checkout.

``testdata/keyword-symbolic-names/resolutions.tsv`` -- executed (not
EXPLAINed) queries for the ambiguous spellings, columns ``id query
neo4j_result relate_result``. It records how forms such as
``RETURN distinct``, ``RETURN not``, ``count(distinct)``, an unquoted
``true``/``null``/``inf``/``nan`` reference and the ``(WHERE ...)`` /
``[WHERE ...]`` element patterns resolve on each engine.

Usage
-----

Full live capture. The script starts its own ``neo4j-kw-probe`` container on
port 7689, waits for it to report online, drives it with the ``neo4j``
Python driver, writes both fixtures, and removes the container again::

    python3 scripts/keyword_symbolic_name_probe.py --write

The exact ``docker run`` command it uses (for a manual container to drive
with ``--no-docker``)::

    docker run -d --name neo4j-kw-probe -p 127.0.0.1:7689:7687 \
        -e NEO4J_AUTH=neo4j/password123 neo4j:2026.09-community

Recompute ``gap_summary`` and ``over_acceptance`` from the committed
``probe.tsv`` -- no Docker, no Neo4j, no Relate binary needed. This is
the reproducibility check for the two recorded outputs::

    python3 scripts/keyword_symbolic_name_probe.py --summary

``--summary`` also validates the fixture: it must carry the header below,
unique ids, the four group names, exactly the shapes defined in this script
and every spelling derived from ``token.rs`` for every shape.
"""

from __future__ import annotations

import argparse
import contextlib
import csv
import json
import re
import subprocess
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
TOKEN_RS = REPO_ROOT / "relate" / "relate-parser" / "src" / "token.rs"
FIXTURE_DIR = REPO_ROOT / "testdata" / "keyword-symbolic-names"
PROBE_TSV = FIXTURE_DIR / "probe.tsv"
RESOLUTIONS_TSV = FIXTURE_DIR / "resolutions.tsv"
DEFAULT_CLI = REPO_ROOT / "target" / "debug" / "relate"

PROBE_HEADER = ["id", "group", "shape", "keyword", "neo4j", "relate", "query"]
RESOLUTIONS_HEADER = ["id", "query", "neo4j_result", "relate_result"]
GROUPS = ["alias", "reference", "binding", "key"]

# The provenance comment block `write_tsv` emits above the resolutions.tsv
# header, so `--write` reproduces the committed file byte-for-byte (the
# writer owns the preamble; nothing is re-added by hand). Kept non-volatile
# (no capture date / fetched version) so a re-run of `--write` against the
# same engines only diffs on real outcome changes.
RESOLUTIONS_PREAMBLE = [
    "# Provenance: relate_result from target/debug/relate (:memory:, no EXPLAIN) built",
    "# from this checkout; neo4j_result from the --image default (neo4j:2026.09-community).",
    "# Regenerate the full fixture (both columns): python3 scripts/keyword_symbolic_name_probe.py --write",
    "# (--write rewrites this file including this preamble; nothing is re-added by hand).",
]

# One entry per probed position: (group, shape slug, query template).
# `$k` is replaced by the keyword spelling exactly as `lookup_keyword` spells
# it. Each row exercises exactly one position: alias rows bind the keyword
# unquoted and never refer to it; reference rows bind it backticked and refer
# to it unquoted once; binding rows bind it unquoted and refer to it only
# backticked; key rows use it as a map/pattern key.
SHAPES: list[tuple[str, str, str]] = [
    # alias: `AS <keyword>`, never referred to.
    ("alias", "alias-return", "RETURN 1 AS $k"),
    ("alias", "alias-with", "WITH 1 AS $k RETURN 1"),
    ("alias", "alias-unwind", "UNWIND [1] AS $k RETURN 1"),
    ("alias", "alias-load-csv", "LOAD CSV FROM 'file:///x.csv' AS $k RETURN 1"),
    ("alias", "alias-yield", "CALL db.labels() YIELD label AS $k RETURN 1"),
    # reference: bound backticked, referred to unquoted once.
    ("reference", "ref-return-bare", "WITH 1 AS `$k` RETURN $k"),
    ("reference", "ref-with-first", "WITH 1 AS `$k` WITH $k RETURN 1"),
    ("reference", "ref-with-after-comma", "WITH 1 AS `$k`, 2 AS x WITH x, $k RETURN 1"),
    ("reference", "ref-arithmetic", "WITH 1 AS `$k` RETURN $k + 1"),
    ("reference", "ref-function-arg", "WITH 1 AS `$k` RETURN abs($k)"),
    ("reference", "ref-aggregate-arg", "WITH 1 AS `$k` RETURN count($k)"),
    ("reference", "ref-where", "WITH 1 AS `$k` WHERE $k = 1 RETURN 1"),
    ("reference", "ref-order-by", "WITH 1 AS `$k` RETURN 1 AS x ORDER BY $k"),
    ("reference", "ref-property-base", "WITH {p: 1} AS `$k` RETURN $k.p"),
    ("reference", "ref-subscript-base", "WITH [1, 2] AS `$k` RETURN $k[0]"),
    ("reference", "ref-list-element", "WITH 1 AS `$k` RETURN [$k, 2]"),
    ("reference", "ref-comparison", "WITH 1 AS `$k` RETURN $k < 2"),
    # binding: bound unquoted, referred to only backticked.
    ("binding", "bind-node", "MATCH ($k) RETURN `$k`"),
    ("binding", "bind-relationship", "MATCH ()-[$k]->() RETURN `$k`"),
    ("binding", "bind-path", "MATCH $k = ()-->() RETURN `$k`"),
    ("binding", "bind-foreach", "FOREACH ($k IN [1] | CREATE (n {v: `$k`}))"),
    ("binding", "bind-list-comprehension", "RETURN [$k IN [1] | `$k`]"),
    ("binding", "bind-pattern-comprehension", "RETURN [($k)-->() | `$k`]"),
    ("binding", "bind-quantifier", "RETURN all($k IN [1] WHERE `$k` = 1)"),
    ("binding", "bind-reduce", "RETURN reduce($k = 0, x IN [1] | `$k` + x)"),
    # The import list is the unquoted binding occurrence under test; the
    # outer scope binding and the subquery reference stay backticked.
    (
        "binding",
        "bind-call-import",
        "WITH 1 AS `$k` CALL ($k) { RETURN `$k` AS y } RETURN y",
    ),
    # key: the keyword spelling is the map / pattern key under test.
    ("key", "key-map-literal", "RETURN {$k: 1} AS m"),
    ("key", "key-map-projection", "WITH {x: 1} AS n RETURN n{$k: 1} AS m"),
    ("key", "key-map-projection-prop", "WITH {$k: 1} AS n RETURN n{.$k} AS m"),
    ("key", "key-map-projection-variable", "WITH 1 AS `$k`, {x: 2} AS n RETURN n{$k} AS m"),
    ("key", "key-pattern-property-map", "MATCH (n {$k: 1}) RETURN n"),
]

# Executed resolution rows: (id, query). These are run as written, no
# EXPLAIN, so the result shows how each ambiguous form resolves.
RESOLUTIONS: list[tuple[str, str]] = [
    ("res-null-ref", "WITH 1 AS `null` RETURN null AS r"),
    ("res-true-ref", "WITH 1 AS `true` RETURN true AS r"),
    ("res-false-ref", "WITH 1 AS `false` RETURN false AS r"),
    ("res-inf-ref", "WITH 1 AS `inf` RETURN inf AS r"),
    ("res-nan-ref", "WITH 1 AS `nan` RETURN nan AS r"),
    ("res-return-distinct", "RETURN distinct"),
    ("res-with-distinct", "WITH distinct RETURN 1"),
    ("res-count-distinct", "RETURN count(distinct)"),
    ("res-return-not", "RETURN not"),
    ("res-all-ref", "WITH 1 AS `all` RETURN all AS r"),
    ("res-case-ref", "WITH 1 AS `case` RETURN case AS r"),
    ("res-count-ref", "WITH 1 AS `count` RETURN count AS r"),
    ("res-exists-ref", "WITH 1 AS `exists` RETURN exists AS r"),
    ("res-collect-ref", "WITH 1 AS `collect` RETURN collect AS r"),
    # Element patterns, without and with a keyword variable.
    ("res-node-where-anon", "MATCH (WHERE 1 = 1) RETURN 1 AS r"),
    ("res-node-where-kwvar", "MATCH (where WHERE true) RETURN `where` AS r"),
    ("res-rel-where-anon", "MATCH ()-[WHERE true]->() RETURN 1 AS r"),
    ("res-rel-where-kwvar", "MATCH ()-[where WHERE true]->() RETURN `where` AS r"),
]


# ---------------------------------------------------------------------------
# keyword derivation
# ---------------------------------------------------------------------------


def derive_keyword_spellings(token_rs: Path = TOKEN_RS) -> list[str]:
    """Every spelling the one keyword-vocabulary declaration maps to a keyword token.

    Parses the ``declare_keywords!`` invocation in ``token.rs``: each pair
    line is ``b"SPELLING" => TokenType::Variant,`` (aliases included --
    ``ASC`` and ``ASCENDING`` are two pairs), so every byte-string literal
    before a ``=>`` is one spelling, in source order (stable across runs).
    """
    lines = token_rs.read_text().splitlines()
    start = next(
        (i for i, line in enumerate(lines) if line.startswith("declare_keywords! {")),
        None,
    )
    if start is None:
        raise SystemExit(f"declare_keywords! invocation not found in {token_rs}")
    # The invocation ends at the first column-0 closing brace.
    end = next(i for i in range(start + 1, len(lines)) if lines[i] == "}")
    body = "\n".join(lines[start + 1 : end])
    spellings = re.findall(r'b"([^"]+)"\s*=>', body)
    if not spellings:
        raise SystemExit(f"no keyword spellings parsed from {token_rs}")
    return spellings


# ---------------------------------------------------------------------------
# outcome classification
# ---------------------------------------------------------------------------


def first_line(text: str) -> str:
    """First non-empty line of `text`, trimmed, tab/newline-free."""
    for line in text.splitlines():
        line = line.strip()
        if line:
            return line.replace("\t", " ")
    return ""


def classify_neo4j(exc: BaseException) -> str:
    """`syntax_error` for a Neo4j syntax error, else `other_error:<code>`."""
    code = getattr(exc, "code", "") or type(exc).__name__
    gql_status = getattr(exc, "gql_status", "") or ""
    if code == "Neo.ClientError.Statement.SyntaxError" or gql_status.startswith("42N"):
        return "syntax_error"
    return f"other_error:{code}"


def classify_relate(proc: subprocess.CompletedProcess) -> str:
    """Exit 0 -> accepted; stderr says `syntax error`; else `other_error:<line>`."""
    if proc.returncode == 0:
        return "accepted"
    if "syntax error" in proc.stderr:
        return "syntax_error"
    return f"other_error:{first_line(proc.stderr)}"


# ---------------------------------------------------------------------------
# engine runners
# ---------------------------------------------------------------------------


def run_relate(cli: Path, query: str, explain: bool) -> subprocess.CompletedProcess:
    statement = f"EXPLAIN {query}" if explain else query
    return subprocess.run(
        [str(cli), ":memory:", statement],
        capture_output=True,
        text=True,
        timeout=120,
    )


def neo4j_explain(session, query: str) -> str:
    from neo4j.exceptions import Neo4jError

    try:
        session.run("EXPLAIN " + query).consume()
        return "accepted"
    except Neo4jError as exc:
        return classify_neo4j(exc)


def neo4j_execute(session, query: str) -> str:
    from neo4j.exceptions import Neo4jError

    try:
        rows = session.run(query).data()
        return "ok: " + json.dumps(rows, default=str)
    except Neo4jError as exc:
        return "err: " + first_line(str(exc))


def relate_cli_execute(cli: Path, query: str) -> str:
    proc = run_relate(cli, query, explain=False)
    if proc.returncode == 0:
        out = proc.stdout.strip()
        return "ok: " + (" | ".join(out.splitlines()) if out else "[]")
    return "err: " + first_line(proc.stderr)


# ---------------------------------------------------------------------------
# live capture
# ---------------------------------------------------------------------------


def start_container(container: str, image: str, port: int, password: str) -> None:
    subprocess.run(["docker", "rm", "-f", container], capture_output=True)
    subprocess.run(
        [
            "docker",
            "run",
            "-d",
            "--name",
            container,
            "-p",
            f"127.0.0.1:{port}:7687",
            "-e",
            f"NEO4J_AUTH=neo4j/{password}",
            image,
        ],
        check=True,
        capture_output=True,
        text=True,
    )


def wait_for_online(driver, timeout: float = 240.0) -> None:
    """Block until the default database answers `RETURN 1`."""
    deadline = time.monotonic() + timeout
    last: BaseException | None = None
    while time.monotonic() < deadline:
        try:
            with driver.session() as session:
                session.run("RETURN 1").consume()
            return
        except Exception as exc:  # noqa: BLE001 - server not up yet
            last = exc
            time.sleep(2)
    raise SystemExit(f"neo4j did not come online within {timeout:.0f}s: {last}")


def fetch_neo4j_version(driver) -> str:
    with driver.session() as session:
        rows = session.run(
            "CALL dbms.components() YIELD name, versions, edition "
            "RETURN name, versions, edition"
        ).data()
    kernel = next(row for row in rows if row["name"] == "Neo4j Kernel")
    edition = kernel.get("edition") or ""
    version = kernel["versions"][0]
    return f"{version} {edition.capitalize()}".strip()


def capture_probe(driver, cli: Path, spellings: list[str]) -> list[dict[str, str]]:
    rows: list[dict[str, str]] = []
    with driver.session() as session:
        for group, shape, template in SHAPES:
            for keyword in spellings:
                query = template.replace("$k", keyword)
                neo4j_outcome = neo4j_explain(session, query)
                relate_outcome = classify_relate(run_relate(cli, query, True))
                rows.append(
                    {
                        "id": f"{shape}/{keyword}",
                        "group": group,
                        "shape": shape,
                        "keyword": keyword,
                        "neo4j": neo4j_outcome,
                        "relate": relate_outcome,
                        "query": query,
                    }
                )
            print(f"captured {shape} ({len(rows)} rows)", file=sys.stderr)
    return rows


def capture_resolutions(driver, cli: Path) -> list[dict[str, str]]:
    rows: list[dict[str, str]] = []
    with driver.session() as session:
        for resolution_id, query in RESOLUTIONS:
            rows.append(
                {
                    "id": resolution_id,
                    "query": query,
                    "neo4j_result": neo4j_execute(session, query),
                    "relate_result": relate_cli_execute(cli, query),
                }
            )
    return rows


# ---------------------------------------------------------------------------
# fixture IO and validation
# ---------------------------------------------------------------------------


def sanitize(value: str) -> str:
    return value.replace("\t", " ").replace("\n", " ").replace("\r", " ")


def write_tsv(
    path: Path,
    header: list[str],
    rows: list[dict[str, str]],
    preamble: list[str] | None = None,
) -> None:
    with path.open("w", newline="") as handle:
        for line in preamble or []:
            handle.write(line + "\n")
        writer = csv.writer(handle, delimiter="\t", lineterminator="\n")
        writer.writerow(header)
        for row in rows:
            writer.writerow([sanitize(row[column]) for column in header])


def read_tsv(path: Path, header: list[str]) -> list[dict[str, str]]:
    with path.open(newline="") as handle:
        reader = csv.reader(handle, delimiter="\t")
        try:
            got_header = next(reader)
        except StopIteration:
            raise SystemExit(f"{path} is empty")
        if got_header != header:
            raise SystemExit(f"{path}: header {got_header} != {header}")
        rows = []
        for line_number, fields in enumerate(reader, start=2):
            if len(fields) != len(header):
                raise SystemExit(
                    f"{path}:{line_number}: expected {len(header)} columns, got {len(fields)}"
                )
            rows.append(dict(zip(header, fields)))
    return rows


def validate_probe(rows: list[dict[str, str]], spellings: list[str]) -> None:
    problems: list[str] = []
    seen: dict[str, dict[str, str]] = {}
    for row in rows:
        row_id = row["id"]
        if row_id in seen:
            problems.append(f"duplicate id: {row_id}")
        seen[row_id] = row
        if row["group"] not in GROUPS:
            problems.append(f"{row_id}: bad group {row['group']!r}")
        if "\t" in row["query"] or "\n" in row["query"]:
            problems.append(f"{row_id}: query is not a single tab-free line")
    expected_shapes = {shape for _, shape, _ in SHAPES}
    actual_shapes = {row["shape"] for row in rows}
    for shape in sorted(expected_shapes - actual_shapes):
        problems.append(f"missing shape: {shape}")
    for shape in sorted(actual_shapes - expected_shapes):
        problems.append(f"unknown shape: {shape}")
    for _, shape, _ in SHAPES:
        for keyword in spellings:
            if f"{shape}/{keyword}" not in seen:
                problems.append(f"missing row: {shape}/{keyword}")
    if problems:
        for problem in problems[:40]:
            print(f"ERROR: {problem}", file=sys.stderr)
        if len(problems) > 40:
            print(f"ERROR: ... and {len(problems) - 40} more", file=sys.stderr)
        raise SystemExit(1)


# ---------------------------------------------------------------------------
# summary
# ---------------------------------------------------------------------------


def compute_gap_summary(rows: list[dict[str, str]]) -> dict[str, dict[str, list[str]]]:
    """group -> shape -> spellings Neo4j accepts and Relate syntax-rejects."""
    gap: dict[str, dict[str, list[str]]] = {
        group: {shape: [] for g, shape, _ in SHAPES if g == group} for group in GROUPS
    }
    for row in rows:
        if row["neo4j"] == "accepted" and row["relate"] == "syntax_error":
            gap[row["group"]][row["shape"]].append(row["keyword"])
    return {
        group: {shape: sorted(keywords) for shape, keywords in shapes.items()}
        for group, shapes in gap.items()
    }


def compute_over_acceptance(rows: list[dict[str, str]]) -> list[str]:
    """Row ids Relate accepts while Neo4j rejects them."""
    return sorted(
        row["id"] for row in rows if row["relate"] == "accepted" and row["neo4j"] != "accepted"
    )


# ---------------------------------------------------------------------------
# commands
# ---------------------------------------------------------------------------


def cmd_write(args: argparse.Namespace) -> int:
    cli = Path(args.relate) if args.relate else DEFAULT_CLI
    if not cli.exists():
        raise SystemExit(
            f"relate binary not found: {cli} (run `cargo build -p relate-cli`)"
        )

    from neo4j import GraphDatabase  # lazy: only live capture needs the driver

    spellings = derive_keyword_spellings()
    print(f"derived {len(spellings)} keyword spellings from {TOKEN_RS}", file=sys.stderr)

    if not args.no_docker:
        start_container(args.container, args.image, args.port, args.password)
    try:
        with contextlib.closing(
            GraphDatabase.driver(
                args.uri,
                auth=(args.user, args.password),
                notifications_min_severity="OFF",
            )
        ) as driver:
            wait_for_online(driver)
            version = fetch_neo4j_version(driver)
            probe_rows = capture_probe(driver, cli, spellings)
            resolution_rows = capture_resolutions(driver, cli)
    finally:
        if not args.no_docker:
            subprocess.run(["docker", "rm", "-f", args.container], capture_output=True)

    write_tsv(PROBE_TSV, PROBE_HEADER, probe_rows)
    write_tsv(
        RESOLUTIONS_TSV,
        RESOLUTIONS_HEADER,
        resolution_rows,
        preamble=RESOLUTIONS_PREAMBLE,
    )
    print(f"wrote {PROBE_TSV} ({len(probe_rows)} rows)", file=sys.stderr)
    print(f"wrote {RESOLUTIONS_TSV} ({len(resolution_rows)} rows)", file=sys.stderr)
    print(
        json.dumps(
            {
                "neo4j_version": version,
                "probe_rows": len(probe_rows),
                "resolutions_rows": len(resolution_rows),
                "gap_summary": compute_gap_summary(probe_rows),
                "over_acceptance": compute_over_acceptance(probe_rows),
            },
            indent=2,
        )
    )
    return 0


def cmd_summary(args: argparse.Namespace) -> int:
    spellings = derive_keyword_spellings()
    rows = read_tsv(PROBE_TSV, PROBE_HEADER)
    validate_probe(rows, spellings)
    print(
        json.dumps(
            {
                "gap_summary": compute_gap_summary(rows),
                "over_acceptance": compute_over_acceptance(rows),
            },
            indent=2,
        )
    )
    print(
        f"validated {len(rows)} rows against {len(spellings)} spellings x {len(SHAPES)} shapes",
        file=sys.stderr,
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument(
        "--write",
        action="store_true",
        help="live capture: start the neo4j container, write both fixtures, remove it",
    )
    mode.add_argument(
        "--summary",
        action="store_true",
        help="recompute gap_summary and over_acceptance from the committed probe.tsv",
    )
    parser.add_argument("--uri", default="bolt://localhost:7689", help="Neo4j Bolt URI")
    parser.add_argument("--user", default="neo4j", help="Neo4j user")
    parser.add_argument("--password", default="password123", help="Neo4j password")
    parser.add_argument("--container", default="neo4j-kw-probe", help="Docker container name")
    parser.add_argument("--image", default="neo4j:2026.09-community", help="Neo4j Docker image")
    parser.add_argument("--port", type=int, default=7689, help="Host port for Bolt")
    parser.add_argument(
        "--no-docker",
        action="store_true",
        help="do not manage the container; capture against an already-running --uri",
    )
    parser.add_argument(
        "--relate",
        default=None,
        help="relate binary to probe (default: target/debug/relate)",
    )
    args = parser.parse_args()

    if args.write:
        return cmd_write(args)
    return cmd_summary(args)


if __name__ == "__main__":
    sys.exit(main())
