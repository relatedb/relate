---
type: adr
area: surface
name: 0213-python-driver-result-ergonomics
status: proposed
tags:
  - python
title: Python driver-like result ergonomics — Record rows, single/peek/data, to_df
REFERENCES:
  - "[[adr/0210-call-in-transactions-batching]]"
  - "[[adr/0214-python-result-graph]]"
---

# ADR-0213: Python driver-like result ergonomics — Record rows, single/peek/data, to_df()

## Context

LABS-253. The Python surface returns plain `dict` rows and is DB-API-complete,
but lacks the Neo4j Python driver's result conveniences (verified against
`neo4j` 6.4.0): `Record`-style rows (attribute access, `.data()`),
`result.data()`, `single()`, `peek()`, `fetch(n)`, `to_df()`. Callers (the
Stack Overflow notebook, agents) re-implement these by hand.

Constraints from the ticket:

- Row type change is a public-API change → needs this ADR; it must not break
  the existing DB-API contract (rows are mappings).
- DataFrames build on the engine's Arrow path (`execute_arrow`) conceptually,
  but a cursor's rows are already materialized — re-running the query to get
  Arrow bytes would be wrong. The frame is built from the held rows with the
  frame library's own from-rows constructor.
- pandas only for now (driver parity), as an optional extra
  (`neo4j-cypherlite[pandas]`); the base wheel stays dependency-free, with the
  same helpful ImportError style as `cypherlite.viz`.

## Decision

1. **`Record`** (`cypherlite.Record`): a `dict` subclass with attribute access
   (`r.name`), `.data()` (plain-dict copy), `.keys()`, and iteration in column
   order. Cursors yield `Record` everywhere a row comes out (`fetchone`,
   `fetchmany`, `fetchall`, iteration, `single`, `peek`). `dict` equality and
   `isinstance` keep working, so existing code and the existing test suite are
   unaffected.
2. **Cursor result conveniences** (driver parity):
   - `cursor.data()` — all remaining rows as a list of `Record`s (consumes).
   - `cursor.single()` — the one record; raises the new
     `ResultNotSingleError` (a `ProgrammingError` subclass) on zero or more
     than one remaining row, like the driver's `Result.single()`.
   - `cursor.peek()` — the next record without advancing, `None` at the end.
   - `cursor.fetch(n)` — alias of `fetchmany(n)` (the driver's spelling).
   - `cursor.keys()` — alias of `.columns` (the driver's naming).
3. **`cursor.to_df()`** — remaining rows as a `pandas.DataFrame`
   (`pd.DataFrame(rows)`, consuming like `fetchall`). Raises `ImportError`
   with install instructions (`pip install neo4j-cypherlite[pandas]`) when
   pandas is absent — same pattern as `cypherlite.viz`. Polars/narwhals are
   deliberately out of scope (see Alternatives).
4. Not in scope: `consume()`/`ResultSummary` (the engine's stats carry no
   statement timing; a summary object without timings is a hollow copy),
   `graph()` (covered by `cypherlite.viz.to_visualization_graph`, which
   already accepts materialized results).

## Alternatives considered

- **Keep plain dict rows.** Rejected: the ticket's core ask is driver-style
  rows; a dict subclass adds them without breaking the DB-API contract.
- **`to_df()` through `execute_arrow` (re-query for Arrow IPC).** Rejected:
  the cursor's result is already materialized; re-running the query is a
  semantic surprise (data may have changed) and doubles the engine work. The
  Arrow path stays for `Database.execute_arrow` users who want engine→Arrow
  without intermediate dicts.
- **polars/narwhals instead of or alongside pandas.** Rejected for now:
  pandas is what the driver and the audience use; the `to_df` seam is small
  and a polars variant can land later without changing the pandas contract.
- **A `consume()` summary object with fake timings.** Rejected: no engine-side
 statement timing exists today ([ADR-0210](./0210-call-in-transactions-batching.md) batching reports per-batch; a
  statement-level summary needs an engine decision first).

## Consequences

- `cypherlite/cursor.py` yields `Record` rows; `cypherlite/exceptions.py`
  gains `ResultNotSingleError`; `pyproject.toml` gains the `pandas` extra.
- Spec/description/example updates under `docs/api/python/` and the
  client-python skill reference land in the same change.
- The Stack Overflow notebook and any DB-API client code keep working
  unchanged (Record is a dict).
- A statement-level `consume()`/`ResultSummary` remains future work pending an
  engine-side timing decision.

## Later references

- [ADR-0214](./0214-python-result-graph.md) supersedes this ADR's "not in scope" note on `graph`:
  `Cursor.graph()` returns a de-duplicated `ResultGraph`, with
  driver-parity `element_id` / `items()` on `Node` and `Relationship` for
  neo4j-viz interop.
- The same LABS-253 change set adds `Cursor.to_arrow()` (rows as a
  `pyarrow.Table` via the existing `[arrow]` extra — the dependency-light
  bulk path alongside `to_df()`) and ships `py.typed` (PEP 561) so type
  checkers see the annotations; both extend this ADR's ergonomics intent.
