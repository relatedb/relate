# ADR-0526: Query engine seam

Status: Proposed
Claim: 29fa3907d6121880c5e1073f08b90c0f4cad32ac

## Context

Execution, the live graph, and the indexes are one crate, and the
storage crate depends on that crate to checkpoint and recover. The
planner already costs plans through a statistics trait rather than the
live layout. A second product runtime was withdrawn because two
runtimes split language coverage (ADR-0052).

A later columnar or paged layout needs its own execution and its own
graph and index storage, without a second language implementation and
without edits inside the in-memory engine.

## Decision

The parser, the front end, the IR, and the planner stay shared. A query
engine owns physical execution and the layout of the graph and of the
indexes. The planner keeps choosing plans from the statistics the
engine supplies.

The facade calls an engine once per statement and once per commit. It
does not call it per row. The in-memory engine is the only
implementation. Selecting an engine stays inside the facade until a
second engine exists, so the public open options do not change.

Snapshot codecs and recovery talk to an export of a snapshot and an
import into an empty engine. They do not name the in-memory graph type.

Expression evaluation and the function and procedure registries stay
inside the in-memory engine until a second engine needs them shared.
That extraction is follow-up work, not part of this seam.

This is one facade and one language front end with a replaceable
engine. It is not a second product runtime.

## Alternatives considered

- **A second runtime beside the product** — rejected: two runtimes
  split language coverage, which is why ADR-0052 withdrew the previous
  one.
- **A trait per operator, called per row** — rejected: the hot path
  would pay for dynamic dispatch, and a transaction's scratch type
  would leak across the boundary.
- **Swapping the planner with the engine** — rejected: each engine
  would reimplement the language and the suites would stop being one
  contract.

## Consequences

- A contract crate holds the shared types and the engine traits. The
  storage crate depends on that contract, not on the executor.
- The store manifest records the query engine identity. A store written
  before this change opens as the in-memory engine. Opening a store
  with a different identity fails.
- The existing latency and memory gates are the check that the boundary
  did not slow the in-memory engine.
- The first alternate query engine has to pull expression evaluation
  into the shared contract before it can pass the language suites.
