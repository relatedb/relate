# 0210. `CALL { … } IN TRANSACTIONS`: legacy WITH-import, concluding results, per-batch commits

- **Status**: Proposed
- **Date**: 2026-10-11
- **Deciders**: CypherLite maintainers (Linear [LABS-264](https://linear.app/neo4j/issue/LABS-264))
- **Related**: ADR-0016 (optimistic write overlay), ADR-0019 (group
  commit), ADR-0172 (incremental constraint validation), ADR-0173
  (overlay-owned redo log), ADR-0287 (NEXT unit output). LABS-263
  (internal batching of global bulk ops) is parallel work; this ADR is
  scoped to the `CALL { … } IN TRANSACTIONS` surface only.

## Context

`CALL { … } IN TRANSACTIONS [OF N ROWS]` parses and plans today, and the
executor already splits outer rows into batches with a per-batch scratch
snapshot/rollback (`exec_call_in_transactions`). Probing the build shows
three defects:

1. **The standard importing-WITH form is rejected.**
   `MATCH (n) CALL { WITH n DETACH DELETE n } IN TRANSACTIONS OF 10 ROWS`
   fails with `VariableAlreadyBound: Variable 'n' already declared in
   outer scope`. The legacy-import collision check in
   `cypherlite-front`'s `validate/scope.rs` asks
   `compute_subquery_output_vars` for the body's terminal projection, but
   `local_stage_output_vars` never clears the projection when a
   non-projection write clause (`DELETE`/`SET`/`REMOVE`/`FOREACH`)
   follows the importing `WITH`. The importing `WITH` is therefore
   mistaken for the terminal projection, and the imported name is
   reported as re-exported. A write-only body exports nothing, so no
   collision can exist.

2. **A concluding `CALL { … } IN TRANSACTIONS` returns no rows.**
   `CALL { MATCH (n) RETURN n } IN TRANSACTIONS` executes the body (rows
   are produced per batch) but the statement result is empty:
   `finish_part` (and its read-path twin `execute_part_read_seeded`)
   project a concluding `CALL` only for procedure calls
   (`Clause::Call`), not for subqueries (`Clause::CallSubquery`), so the
   part finishes with an empty result.

3. **Batching does not bound memory.** Every batch's writes accumulate
   in the single statement `WriteScratch` (entities, adjacency, redo
   log) until the outer statement commits once at the end. The
   `MATCH (n) CALL { WITH n MATCH (m) CREATE (n)-[:TAGGED]->(m) }
   IN TRANSACTIONS` bulk-load idiom therefore grows scratch memory (and
   overlay merge cost) with the *total* write volume, not the batch
   size — on a 417-node store the 173,889-rel cross-product load is the
   LABS-263 repro shape, and batching it changes nothing about peak
   memory today.

The pinned openCypher TCK corpus contains no `IN TRANSACTIONS`
scenarios, so no allowlist change is involved.

## Decision

### 1. Terminal-projection walk skips non-projection write clauses

In `local_stage_output_vars`
(`cypherlite-front/src/validate/scope.rs`), `Clause::Delete`,
`Clause::Set`, `Clause::Remove` and `Clause::Foreach` reset the tracked
terminal projection to `None`, exactly as `Clause::Create` and
`Clause::Merge` already do. A `WITH`/`RETURN` later in the body still
establishes a projection, so `CALL { WITH n RETURN n }` keeps raising
`VariableAlreadyBound`, while a body whose last projection is followed
only by writes exports nothing and the spurious collision disappears.
This is the parser-visible scoping rule for every `CALL { }` form, not
only `IN TRANSACTIONS`.

### 2. A concluding `CALL { … }` projects the body's exported columns

`finish_part` and `execute_part_read_seeded` share one concluding-CALL
helper that also covers `Clause::CallSubquery`: when the concluding
subquery body has a terminal `RETURN`, the statement result is the
produced rows projected to the body's exported columns
(`call_export_columns`). A `REPORT STATUS AS <var>` column never
appears here: the planner rejects REPORT STATUS without a following
RETURN, so the status column only surfaces through that trailing
RETURN. A unit/write-only body (no terminal `RETURN`) exports nothing
and the statement result stays empty, matching today's behavior for
that shape. Outer variables in scope before the `CALL` are not added to
the result columns.

### 3. Per-batch commits fold each batch into the base graph

`IN TRANSACTIONS` semantics are independent inner transactions: a
committed batch stays committed even when a later batch fails under
`ON ERROR FAIL`. The executor gains a batch-commit step: after a batch
succeeds (constraint validation included), its redo ops are drained
from the statement scratch and committed through a `BatchCommit` sink
the facade injects into the execution request; the scratch is then
rebased to a fresh `WriteScratch` at the new commit timestamp, carrying
the accumulated statement `QueryStats` forward so result statistics
still cover the whole statement. Live scratch state — entity maps,
adjacency indexes, redo log — is bounded by one batch instead of the
whole statement, and later batches read earlier batches' writes from
the base graph.

The facade (`commit_optimistic`) implements the sink by running the
batch's ops through the existing group-commit pipeline
(`try_wal_commit`) with the batch's own txn id and the scratch's
snapshot timestamp, so every batch is WAL-logged and fsync'd per the
configured durability mode before the next batch starts. Statements
containing `IN TRANSACTIONS` therefore do **not** hold the publication
read guard across execution (the group commit needs the publication
write lock; holding a read guard on the same thread would deadlock).
The statement's snapshot pin — which already outlives the read guard
and keeps the `GraphCell` from being replaced (ADR-0137) — plus the
store's timestamped version retention (pins define the GC horizon) keep
mid-execution reads sound while own batch commits apply at newer
timestamps. The statement-level remainder after the last batch (usually
empty) commits as today, conflict-checked against the last batch's
commit timestamp.

When no sink is present — the explicit-transaction path
(`query_in_txn`) — batching keeps today's accumulate-in-scratch
behavior; per-batch commits are an auto-commit-statement feature.

### Scope boundary with LABS-263

LABS-263 owns detection/internal batching of global bulk operations in
`cypherlite-executor`. This change touches only the
parser/front/planner/executor `CALL … IN TRANSACTIONS` paths and the
facade commit entry; it does not alter global-bulk detection.

## Alternatives considered

- **Keep one statement-level commit; only fix the surface bugs.**
  Rejected: it leaves the headline use case — bounding bulk-load memory
  by batching — non-functional; the scratch grows with total writes and
  the overlay merge cost keeps degrading per batch.

- **Apply batches to the base graph in the executor without WAL, then
  WAL-log the whole statement once at the end.** Rejected: it makes
  `REPORT STATUS committed=true` a durability lie (a crash before the
  final commit loses batches the client saw committed) and needs a
  skip-already-applied marker in the group commit's apply phase. The
  sink design reuses the existing WAL path unchanged instead.

- **Drive batching from the facade as N separate auto-commit
  statements.** Rejected: batch boundaries are mid-clause and correlated
  with outer rows; re-evaluating the outer `MATCH` per batch changes
  semantics (e.g. nodes deleted by batch 1 would re-match or vanish in
  batch 2's outer scan).

- **Reject `IN TRANSACTIONS` inside explicit transactions (Neo4j
  parity).** Rejected for this change: current behavior there is the
  pre-existing accumulate-in-scratch semantics; tightening it is a
  separate, user-visible error-behavior decision.

## Consequences

- `MATCH (n) CALL { WITH n DETACH DELETE n } IN TRANSACTIONS OF N ROWS`
  and friends — the standard bulk-delete/bulk-update idiom — parse,
  validate, execute, and return per-batch `REPORT STATUS` rows.
- A query concluding with `CALL { … } [IN TRANSACTIONS]` whose body has
  a terminal `RETURN` returns the body's exported rows instead of an
  empty result. Concluding unit subqueries still return an empty
  result.
- Peak scratch memory and per-batch overlay cost for `IN TRANSACTIONS`
  writes are bounded by the batch size, not the statement's total
  write volume; the LABS-263 cross-product repro (417 nodes → 173,889
  relationships) completes under `IN TRANSACTIONS OF N ROWS`.
- A batch that fails under `ON ERROR FAIL` now leaves earlier batches
  committed (Neo4j `IN TRANSACTIONS` semantics); previously the whole
  statement rolled back. This is an intentional behavior change.
- Each batch is a separate WAL record/fsync group, so batching trades
  one large commit for N smaller durable commits; `OF N ROWS` (default
  1000) controls that granularity.
- While an auto-commit `IN TRANSACTIONS` statement executes, other
  writers' commits may apply between its batches (each batch is a real
  committed transaction); readers and the statement itself remain
  consistent via snapshot timestamps and pins. Concurrent DDL during a
  batched statement is not specially guarded beyond the existing
  commit-time checks.
- `IN TRANSACTIONS` inside an explicit transaction retains the previous
  single-overlay semantics (no per-batch durable commits).

## Later references

- ADR-0570 supersedes the last consequence above: live Neo4j 2026.10.1
  refuses a `CALL { … } IN TRANSACTIONS` statement inside an open explicit
  transaction (GQLSTATUS 25N17) rather than batching into the transaction's
  overlay, and the engine now refuses it at admission. The
  implicit-transaction batch contract recorded here is unchanged.
