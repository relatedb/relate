---
type: adr
area: engine
claim: f350c29020b6d05ecaa90dee16c38d1db1e0405d
name: 0681-in-transactions-plan-observability
status: accepted
tags:
  - executor
  - observability
title: "CALL { … } IN TRANSACTIONS: the body renders as plan children and PROFILE reports the batch facts"
REFERENCES:
  - "[[adr/0669-in-transactions-batch-pass]]"
  - "[[adr/0660-clause-consumer-batch-stream-contract]]"
  - "[[adr/0344-query-plan-nodes]]"
  - "[[adr/0345-plan-wire-form]]"
  - "[[adr/0210-per-batch-commits]]"
---

# ADR-0681: `CALL { … } IN TRANSACTIONS` — the body renders as plan children and PROFILE reports the batch facts

## Context

ADR-0669 gave `CALL { … } IN TRANSACTIONS` its batch pass, and the plan
details already carry the batch fact (`OF n ROWS`), the failure policy
(`ON ERROR …`) and the strategy (`batch pass` / `per-row`). Two facts
remained invisible:

- **The body's operators.** The `CallSubquery` node is opaque — a reader
  cannot see what each batch runs without re-reading the statement text.
  Neo4j's `TransactionForeach` renders the subquery body's operators
  (`Argument` → `Create`) as children of the batched operator.
- **The batches themselves.** PROFILE showed the operator's rows, time and
  db hits, but not the unit the clause actually runs: how many batches
  started, how many committed, how many rolled back under the `ON ERROR`
  policy, and what one batch cost in memory — the facts that say whether
  an authored `OF n ROWS` sized the transactions well.

## Decision

1. **The batched call's body renders as the `CallSubquery` node's plan
   children**, EXPLAIN and PROFILE both. The construction walk that builds
   the statement's tree builds the body's subtree too — stages chained as
   `Next` children, `UNION` parts under a `Union` node — and attaches it
   behind the pipeline's data-flow child (the fused-breakdown position
   rule: the data-flow child stays first). Body children carry no metrics
   of their own: the body runs inside the operator's batch, so its work is
   the operator's record — a per-body-operator row would double-count the
   batch pass's single crossing. A plain `CALL { … }` keeps the opaque
   node: its body is an ordinary subquery of the enclosing statement, not
   a batch the plan names, and widening its plan shape is a separate
   decision.

2. **PROFILE reports the batch facts on the operator**: three counters —
   `batches` (transactions started), `committed`, `rolled_back` (the
   per-batch failure the policy handled, `ON ERROR FAIL`'s failing batch
   included) — and four per-batch maxima: the largest input batch's and
   the largest installed output's retained row bytes, the most scratch
   one batch created (entities, adjacency and property state, the redo
   log excluded), and the most redo-log bytes one batch carried at its
   commit — the WAL payload it wrote, the storage layer's own reserved
   bytes as the honest proxy for what the commit wrote. The facts are
   arm-local, not global counter snapshots: the batch loop notes each
   finished batch into a cell shared with the stream's parked record, so
   the clause's record — reported once the consumer drains or stops the
   stream, after every batch ran — carries them, and an unprofiled run
   never pays a cell write.

   The peaks note from the outcome, not the attempt: a committed batch
   notes all four figures; a rolled-back batch notes the input it held
   and the scratch it created before the restore — never an output it
   never installed nor a WAL payload it never wrote.

3. **The wire group is presence-only**: the seven integers
   (`InTxBatches`, `InTxCommitted`, `InTxRolledBack`,
   `InTxPeakInputRowsBytes`, `InTxPeakOutputRowsBytes`,
   `InTxPeakScratchBytes`, `InTxPeakWalBytes`) emit together, only on a
   PROFILE node whose arm ran batches (`InTxBatches > 0`) — every other
   clause's zeroed facts never appear as a group of zeros. The plan
   table's `Details` cell marks the operator `[in-tx batches=N
   committed=M rolled-back=K]` and `[in-tx-peak input-rows=…
   output-rows=… scratch=… wal=…]`.

4. **The input was already streamed.** The fourth candidate — feeding the
   arm from the upstream in bounded batches instead of a materialized
   input — is settled by ADR-0660's `retain` disposition and ADR-0669's
   arm: the arm pulls its driving stream one batch at a time and holds
   one batch's rows, so its memory is the batch size × row width, never
   the input size. An authored `OF n ROWS` holds n rows because the
   transaction's rollback and report need them — that is the batch, not
   the arm, holding input. Nothing further to decide here.

## Consequences

- EXPLAIN output for a batched call grows a subtree: a reader sees the
  body's operator chain without re-reading the statement; the
  pre-order `Id` numbering and the wire's recursive children form carry
  the deeper tree unchanged (the iterative drop and the plan-wire depth
  budget apply as before).
- Body-clause notifications and warnings (an eager aggregation in a
  body, a cartesian body pattern) now stage with their body nodes and
  surface as statement notifications — they were previously invisible
  inside the opaque node.
- The `Concurrency` column of Neo4j's `IN CONCURRENT TRANSACTIONS`
  output stays out of scope until the `IN CONCURRENT TRANSACTIONS`
  concurrency hint is applied rather than validated; a
  validated-never-applied hint has no runtime fact to report.
- The redo-log reserved bytes are a proxy: the durable WAL record adds
  framing beyond the payload, and a later storage ADR that exposes the
  committed byte count exactly supersedes the figure here.
