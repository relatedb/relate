---
type: adr
area: engine
claim: af100c21d5762f0141de5ea8ce8fd0d6497a2d40
date: 2026-11-23
deciders: Relate maintainers
name: 0669-in-transactions-batch-pass
status: proposed
tags:
  - budget
  - bulk-load
  - executor
title: CALL { … } IN TRANSACTIONS runs one clause-engine pass per batch for write-only bodies
REFERENCES:
  - "[[adr/0210-call-in-transactions-batching]]"
  - "[[adr/0395-nested-evaluation-budget]]"
  - "[[adr/0555-operators-stream-rows-never-accumulate]]"
  - "[[adr/0650-configurable-wal-payload-cap]]"
---

# 0669: CALL { … } IN TRANSACTIONS runs one clause-engine pass per batch for write-only bodies

## Context

`CALL { … } IN TRANSACTIONS` ran its body once per row (`call_subquery_row`
per row of every batch — [ADR-0395](./0395-nested-evaluation-budget.md)'s
recorded counting rule). Two consequences:

- Every row's body pass ticked the [ADR-0395](./0395-nested-evaluation-budget.md)
  nested-evaluation budget (a fixed 100 000 passes per statement, deliberately
  no knob), so the per-commit WAL payload cap's own remedy — *commit the data
  in smaller transactions (`CALL { … } IN TRANSACTIONS OF n ROWS`)*,
  [ADR-0650](./0650-configurable-wal-payload-cap.md) — was impossible to
  follow past 100 000 rows in one statement. A 1 000 000-row bulk load
  refused at the 100 001st pass.
- Each row paid a full nested statement-engine invocation (seed build,
  import-set derivation, result merge) — the dominant cost of a batched bulk
  load, and the reason the batched form is slower than the plain
  `UNWIND … CREATE` it is meant to replace.

The clause engine is natively multi-row (`run_clauses` takes the input rows);
the per-row loop was the anomaly, not the engine.

## Decision

An `IN TRANSACTIONS` body takes the **batch pass** — the batch's seed
rows (built by the same per-row `scope_seed` policy) enter one
`execute_query_seeded` crossing, one nested-budget tick and one
clause-engine invocation per batch — whenever the body is:

- a single flat part with no NEXT stage (`Query::is_single_flat_part`,
  the one IR spelling, also read by the import-invariant shared path);
- non-projecting by the canonical trailing-clause table
  (`stage_projects_rows` over the final stage, whose leaf is the
  planner's `clause_projects_rows` — the same classification the per-row
  merge consults) — never inferred from the clause set: a terminal
  `LOAD CSV` projects rows and stays per-row;
- free of `WITH` and of schema/admin DDL among its own clauses (the
  planner's one-shot `query_any_clause` walk and its published
  `clause_is_ddl_command` predicate — no second vocabulary at the use
  site).

Everything else about the body — `MATCH`, procedure `CALL`, nested
`CALL { … }`, conditionals, graph-reading expressions, pattern
comprehensions — runs in the pass **exactly as its clause list would in
a plain statement over the batch's rows**: pipeline order, the per-row
statement-engine loop never entered. The per-row path remains only for
the shapes that structurally need it:

- a `WITH`-carrying body needs the importing-scope re-attach
  (`run_nested_clauses_scoped`), which re-attaches one scope row per
  nested invocation — per-row by construction (the legacy
  importing-`WITH` form keeps it);
- a projecting body merges each body row back onto its originating
  outer row, a lineage the pass does not track;
- schema/admin DDL has no per-row work to batch.

**The pass deliberately does not preserve Neo4j's per-row interleaving
of reads after writes.** The clause engine runs clause-by-clause over
the batch (pipeline order); Neo4j's nested loop lets row *j*'s read see
rows `< j`'s writes. Within one clause the operator processes rows
sequentially on both paths, so a `MERGE` matching an earlier row's
`MERGE` node is identical; only a read in a later clause observing an
earlier clause's writes from *other* rows differs — and giving that up
is what batching the statement means: the caller names the batch, the
body evaluates it as a unit, and a caller needing per-row observation
runs smaller batches. This is the recorded decision (2026-11-23): the
per-row engine invocation is pure overhead once the commit boundary is
the batch, which [ADR-0210](./0210-call-in-transactions-batching.md)
already made it.

Batch boundaries, per-batch commits, rollback, `ON ERROR`, `REPORT
STATUS`, property defaults and constraint validation
([ADR-0210](./0210-call-in-transactions-batching.md)) are unchanged —
they were already per batch. The counting change is bounded to the
admitted shape; every other `CALL` form keeps
[ADR-0395](./0395-nested-evaluation-budget.md)'s per-row rule.

## Consequences

- The WAL payload cap's named remedy works at any row count: a 100 001-row
  single-statement bulk create through `OF n ROWS` completes instead of
  refusing at the budget, and with [ADR-0670](./0670-nested-eval-budget-one-million.md)'s
  1 000 000-pass budget even the per-row fallback shapes cover
  million-row loads.
- A batched body costs one nested pass per batch — 1 000 000 rows at
  `OF 100 000 ROWS` is ten ticks — and one clause-engine invocation per
  batch: the bulk-load statement forms (`CALL (row) { CREATE … }`,
  `MATCH anchor CALL (a) { MATCH target CREATE rel }`) run at plain-
  statement speed with per-batch WAL records under the cap.
- Reads inside a batched body observe the batch, not the row: pipeline
  order, pinned by test (`read_after_write_body_evaluates_the_batch_as_a_unit`).
  A caller needing Neo4j's per-row interleaving runs smaller batches —
  at `OF 1 ROW` the two paths coincide.
- The admission excludes only the three structural shapes (`WITH`,
  projection, schema DDL). Widening further — per-row scope columns so
  `WITH` bodies can batch, row lineage so projecting bodies can — is
  future work, one widening per demonstrated need. The pass composes
  with [ADR-0660](./0660-clause-consumer-batch-stream-contract.md)'s streaming arm: the
  input is the upstream pull stream, pulled one batch at a time, and the
  arm holds one batch's output (ADR-0555's retain disposition). What
  remains for the streaming follow-up is the pass's own batch crossing —
  the seeds it hands `execute_query_seeded` are the batch, and the
  body's output is discarded in favour of the outer rows.
