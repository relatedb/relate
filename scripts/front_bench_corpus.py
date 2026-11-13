#!/usr/bin/env python3
"""Build the front_bench statement corpus.

Extracts Cypher statements for parser/lowerer profiling
(relate/relate-front/examples/front_bench.rs):

- every statement of every workload file
  testing/relate-query-latency/workloads/**/*.cypher, split on
  top-level semicolons by `split_statements` below (a workload
  file may carry several statements, e.g. graph-benchmark-ldbc's
  `indexes.cypher`); `$x` parameters stay as placeholders
- triple-quoted statement blocks from an openCypher `.feature` corpus whose
  first word is a Cypher statement keyword. The corpus is not in this repo:
  point $TCK_CORPUS at the spec-harness checkout's `features/tck/` (or any
  openCypher checkout's `tck/features`); unset means workload statements
  only, stated in the output, and a named corpus with no feature file is a
  loud failure

Statements are deduplicated by text (workload-first, first occurrence)
and written as a single \\x1e-separated corpus file (default
target/front-bench-corpus.txt), the format front_bench consumes.

The Gherkin reader (`parse_feature`) and the statement splitter
(`split_statements`) below moved here from the verifier script that
retired with the corpora's move to the spec harness (ADR-0520); the
harness checkout's own `scripts/cip_verify.py` is the verifier of
record.

Usage: front_bench_corpus.py [output_path]
"""
import os
import pathlib
import sys
import textwrap
from dataclasses import dataclass, field
from itertools import chain
from typing import Optional

REPO = pathlib.Path(__file__).resolve().parent.parent
OUT = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else REPO / "target/front-bench-corpus.txt"

# The TCK shelf, external by design (the corpus moved to the spec-harness
# repo): no in-repo default can silently stand in for it.
TCK_ENV = "TCK_CORPUS"

KEYWORDS = (
    "MATCH", "OPTIONAL", "RETURN", "WITH", "UNWIND", "CREATE", "MERGE",
    "DELETE", "SET", "REMOVE", "FOREACH", "CALL", "USE", "SHOW", "DROP",
    "LOAD", "ALTER", "START", "STOP", "EXPLAIN", "PROFILE", "SEARCH",
    "INSERT", "WHEN", "FINISH", "PERIODIC",
)

# --------------------------------------------------------------------------
# Gherkin parse layer, relocated from the verifier script that retired
# with the corpora's move to the spec harness (ADR-0520) — the harness
# checkout's copy is the verifier of record
# --------------------------------------------------------------------------

STEP_KEYWORDS = ("Given ", "When ", "Then ", "And ", "But ")


@dataclass
class Step:
    doc_string: Optional[str] = None


@dataclass
class Scenario:
    steps: list[Step] = field(default_factory=list)


@dataclass
class Feature:
    background: list[Step] = field(default_factory=list)
    scenarios: list[Scenario] = field(default_factory=list)


def parse_feature(text: str, source: str = "<feature>") -> Feature:
    """Parse a .feature file into a Feature (dialect of the openCypher TCK).

    The model carries only what the corpus extractor (`tck_statements`)
    reads: the feature/scenario/step walk and each step's doc string —
    step tables and Examples rows are walked past, not modeled.

    A step outside any Background/Scenario is not valid Gherkin, so the
    walk refuses it (naming `source`) rather than dropping it silently."""
    lines = text.splitlines()
    feature = Feature()
    # One sink for the step stream: None before any container, the background
    # list inside a Background block, the open scenario's step list inside
    # a Scenario. The three states are one value, assigned once per
    # container line, so the step branch tests it once.
    sink: Optional[list[Step]] = None
    i = 0

    def parse_step_at(idx: int) -> tuple[Step, int]:
        # The walk already established lines[idx] opens a step; consume
        # the step line, then the doc string and/or table rows that may
        # follow it (in either order), keeping only the doc string.
        step = Step()
        idx += 1
        while idx < len(lines):
            nxt = lines[idx].strip()
            if nxt == '"""':
                idx += 1
                doc_lines: list[str] = []
                while idx < len(lines):
                    if lines[idx].strip() == '"""':
                        idx += 1
                        break
                    doc_lines.append(lines[idx])
                    idx += 1
                # `textwrap.dedent` is the one dedent rule — including its
                # normalization of whitespace-only lines to empty, the
                # load-bearing half a hand-rolled margin walk differed on.
                step.doc_string = textwrap.dedent("\n".join(doc_lines))
                continue
            if nxt.startswith("|"):
                while idx < len(lines) and lines[idx].strip().startswith("|"):
                    idx += 1
                continue
            break
        return step, idx

    while i < len(lines):
        line = lines[i].strip()

        if line.startswith("Background:"):
            sink = feature.background
            i += 1
            continue

        if line.startswith("Scenario Outline:") or line.startswith("Scenario:"):
            scenario = Scenario()
            feature.scenarios.append(scenario)
            sink = scenario.steps
            i += 1
            continue

        if any(line.startswith(kw) for kw in STEP_KEYWORDS):
            if sink is None:
                raise SystemExit(
                    f"{source}: step outside any Background/Scenario: {line}"
                )
            step, i = parse_step_at(i)
            sink.append(step)
            continue

        # A blank line, a comment, a `Feature:`/`Examples:` header, or a
        # table row outside a step: none opens a container or a step, so the
        # walk just advances past it.
        i += 1

    return feature


def split_statements(text: str) -> list[str]:
    """Split statement text on top-level semicolons, tracking single- and
    double-quoted strings for statement-boundary detection in the bench
    corpus: a `;` inside a quoted string never ends a statement, and the
    other quote kind inside a string is an ordinary character. A
    bench-corpus convenience, not parity with the canonical
    `relate-splitter` (Rust), which also models escapes, comments and
    backtick-quoted identifiers."""
    stmts: list[str] = []
    cur: list[str] = []
    # One quote state: None outside a string, the open quote character
    # inside it — at most one string is open at a time.
    quote: Optional[str] = None
    for ch in text:
        if quote is None:
            if ch in ("'", '"'):
                quote = ch
            elif ch == ";":
                stmt = "".join(cur).strip()
                if stmt:
                    stmts.append(stmt)
                cur = []
                continue
        elif ch == quote:
            quote = None
        cur.append(ch)
    stmt = "".join(cur).strip()
    if stmt:
        stmts.append(stmt)
    return stmts


def read_strict(path):
    """Read `path` as UTF-8, failing loud and naming the file — the one
    reader both collectors use, so a malformed source dies with the file
    named instead of smuggling replacement characters into the corpus."""
    try:
        return path.read_text(encoding="utf-8")
    except UnicodeDecodeError as e:
        raise SystemExit(f"{path}: not valid UTF-8: {e}") from e


def workload_statements():
    """Every statement of every workload file, split on top-level
    semicolons by `split_statements` above, never assuming one
    statement per file."""
    root = REPO / "testing/relate-query-latency/workloads"
    out = []
    for f in sorted(root.rglob("*.cypher")):
        out.extend(split_statements(read_strict(f)))
    return out


def tck_root():
    """The corpus directory named by $TCK_ENV, or None when it is unset.

    A named path must be a directory carrying at least one `.feature`
    file: a typo or an empty shelf exits loud instead of contributing an
    empty corpus, the silent-default failure this leg used to risk when
    the corpus lived at a fixed in-repo path."""
    raw = os.environ.get(TCK_ENV)
    if not raw:
        return None
    root = pathlib.Path(raw)
    if not root.is_dir():
        raise SystemExit(
            f"{TCK_ENV}={raw}: not a directory (expected the spec-harness "
            "`features/tck` or an openCypher `tck/features` checkout)"
        )
    if not next(root.rglob("*.feature"), None):
        raise SystemExit(f"{TCK_ENV}={raw}: no .feature file under this directory")
    return root


def tck_statements(root: pathlib.Path) -> list[str]:
    """Every doc-string statement whose first word is a Cypher statement
    keyword. The `\"\"\"` doc-block walk is `parse_feature`'s — this
    file's Gherkin reader — with doc strings dedented exactly as the
    spec harness's TCK consumers read them."""
    out = []
    for f in sorted(root.rglob("*.feature")):
        feature = parse_feature(read_strict(f), source=str(f))
        for step in chain(feature.background, *(sc.steps for sc in feature.scenarios)):
            if step.doc_string is None:
                continue
            statement = step.doc_string.strip()
            if statement and statement.split(None, 1)[0].upper() in KEYWORDS:
                out.append(statement)
    return out


def main():
    wl = workload_statements()
    root = tck_root()
    if root is None:
        print(
            f"tck: skipped — set {TCK_ENV} to an openCypher .feature corpus "
            "(spec-harness `features/tck`, or an openCypher `tck/features`)"
        )
        tck = []
    else:
        tck = tck_statements(root)
    # One dedupe pass over the concatenation, workload-first,
    # first-occurrence order, so the append-if-unseen policy and its
    # precedence live here alone. `sources` is the one spelling of the
    # source vocabulary: the counts dict and the summary line both derive
    # from it. Both per-source counts come off that same pass: a
    # statement's source is the list that contributed it — a statement in
    # both counts as a workload statement, exactly the workload-first
    # order's attribution.
    sources = (("workloads", wl), ("tck", tck))
    statements: list[str] = []
    seen: set[str] = set()
    counts = dict.fromkeys((name for name, _ in sources), 0)
    for name, stmts in sources:
        for stmt in stmts:
            if stmt in seen:
                continue
            seen.add(stmt)
            statements.append(stmt)
            counts[name] += 1
    OUT.parent.mkdir(parents=True, exist_ok=True)
    with OUT.open("w", encoding="utf-8") as fh:
        fh.write("\x1e".join(statements))
    total_bytes = sum(len(s) for s in statements)
    summary = "  ".join(f"{name}: {counts[name]}" for name, _ in sources)
    print(f"{summary}  total: {len(statements)}")
    print(f"total bytes: {total_bytes}")
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
