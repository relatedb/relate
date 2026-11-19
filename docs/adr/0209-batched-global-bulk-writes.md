---
type: adr
area: engine
date: 2026-10-11
deciders: CypherLite maintainers
name: 0209-batched-global-bulk-writes
status: proposed
tags:
  - write-path
title: Batched execution of global bulk writes
REFERENCES:
  - "[[adr/0017-client-conflict]]"
  - "[[adr/0019-group-commit-shared-fsync]]"
  - "[[adr/0111-engine-bounded-execution]]"
  - "[[adr/0172-incremental-constraint-validation]]"
  - "[[adr/0173-overlay-owned-redo-log-and-scratch-index]]"
---

# 0209. Batched execution of global bulk writes

- **Related**: [ADR-0017](./0017-client-conflict.md) (optimistic write conflicts), [ADR-0019](./0019-group-commit-shared-fsync.md) (group
 commit), [ADR-0111](./0111-engine-bounded-execution.md) (bounded execution: cancellation and snapshot pins),
 [ADR-0172](./0172-incremental-constraint-validation.md) (incremental constraint validation), [ADR-0173](./0173-overlay-owned-redo-log-and-scratch-index.md) (redo slices)

## Context

A global bulk write — one whose `MATCH` has no selective anchor and whose
write clause touches every matched entity — runs today as a single giant
transaction: the whole statement executes against one `WriteScratch`, and
its entire redo becomes one WAL record ([ADR-0019](./0019-group-commit-shared-fsync.md)). That breaks down on
real workloads:

- `MATCH (n) DETACH DELETE n` on a 417-node store carrying the ~174k
  relationships of a previous cross-product test deletes ~174k entities
  in one commit: seconds per run, growing with retained version history.
- `MATCH (n) MATCH (m) WITH n AS a, m AS b CREATE (a)-[:TAGGED]->(b)`
  (417×417 ≈ 174k creates) materializes the full redo in memory; at
  larger sizes the single WAL record crosses the 64 MiB payload limit
  (`WAL payload exceeds 64 MiB limit`) and before that the scratch plus
  encoded record can exhaust the process memory (observed OOM, exit 137).
- The same class covers `MATCH (n) SET n.prop = x`,
  `MATCH (n) REMOVE n:Label`, `MATCH ()-[r]->() DELETE r`, and
  multi-label variants.

Neo4j's answer to the same shape is `CALL { … } IN TRANSACTIONS`: the
driver rows are produced once, then applied in bounded batches, each
batch its own transaction. CypherLite already executes that clause by
chunking rows (with per-batch scratch snapshots for its error modes),
but nothing helps the plain statement that the user actually wrote.

## Decision

Auto-commit write statements whose plan matches a conservative "global
bulk" shape are executed in **bounded batches** instead of one giant
transaction. The detection and the per-chunk write logic live in
`cypherlite-executor` (`exec::bulk`); the batch loop lives next to
`commit_optimistic` in `cypherlite` (the only place WAL commits can be
issued).

1. **Eligibility is plan-based and conservative.** A statement is
   batchable only when all of the following hold:

   - auto-commit (no explicit transaction), single-part plan, no UNION,
     no `NEXT`;
   - every `MATCH` is non-optional, carries no `WHERE`/`SEARCH`, and
     consists solely of *bare* patterns: single-node patterns
     `(<var> [:labels…])` (no properties, no element predicates, no
     label expressions, no path variable) or single-relationship
     patterns `()-[<var> [:types…]]->()` (fixed length, no properties,
     no predicates);
   - any `WITH` clauses between the matches and the write are pure
     renames: no aggregation, `DISTINCT`, `ORDER BY`, `SKIP`, `LIMIT`,
     or `WHERE`, and every item is a variable (`WITH *` counts);
   - the statement ends with exactly one write clause and nothing else:
     - `DELETE`/`DETACH DELETE` whose expressions are all matched
       variables (deleting an entity is idempotent; duplicates across
       the cross product are deduplicated by id);
     - `SET` whose targets root at matched variables and whose value
       and dynamic-key expressions reference no matched variable other
       than the target's own (so the per-row effect is independent of
       when sibling rows ran — `SET n.x = n.x + 1` is in,
       `SET n.x = m.x` on a cross product is out);
     - `REMOVE` whose targets root at matched variables and whose
       dynamic keys reference no variables;
     - `CREATE` whose patterns contain no graph-sensitive expressions
       and whose node/relationship property expressions reference only
       matched variables (each combination of matched entities produces
       the same writes regardless of batch boundaries, because the
       matched entity set is snapshotted before the first batch and
       `CREATE` output never feeds back into the match).
 - the catalog holds **no constraints** ([ADR-0172](./0172-incremental-constraint-validation.md)): constraint
     validation is per-transaction, and a mid-statement violation in a
     batched run would leave earlier batches committed. With no
     constraints there is nothing to violate. Statements on a
     constrained catalog take the unbatched path and keep full
     atomicity. The batched run re-checks the catalog at every batch
     and fails the statement if a constraint appeared after phase 1
     (earlier batches stay committed).

2. **Two-phase execution.** Phase 1 pins a snapshot and collects the
   *target identity space* only: one sorted id vector per matched
   variable (node ids filtered by labels, relationship ids filtered by
   types). Memory is O(Σ matched entities), never O(Π) — the
   cross-product shape stores its factors, not its combinations.
   Phase 2 replays the combinations in chunks of bounded *work* (target
   entities plus, for `DETACH DELETE`, their incident relationships —
   estimated from adjacency degrees), each chunk on a fresh scratch
 committed through the normal group-commit path ([ADR-0019](./0019-group-commit-shared-fsync.md)) with the
 normal conflict check ([ADR-0017](./0017-client-conflict.md)) and the statement's cancellation
 token checked per chunk ([ADR-0111](./0111-engine-bounded-execution.md)). Per-chunk writes go through the
   same executor clause logic (`create_patterns`, `apply_sets`,
   `resolve_removes`/`apply_resolved_removes`, `delete_value`) the
   unbatched dispatch uses, on rows re-bound to fresh entity values at
   the chunk's own snapshot.

3. **Small statements keep the existing path.** Below a work threshold
   (10 000 estimated touched entities) the statement runs unbatched, so
   the common case pays no extra commits. The batched path also runs
   only on the auto-commit route; explicit transactions keep their
   boundaries.

4. **Semantics.** On the success path the final store state and the
   statement's write statistics are identical to the unbatched run:
   the match set is fixed before any write (stronger than the unbatched
   read-own-writes order, but these shapes' writes cannot change their
   own match set — the matches bind only bare nodes/relationships and
   the write-clause expressions cannot observe sibling rows). On the
   error path a batched statement may leave earlier batches committed —
   exactly the documented `CALL { } IN TRANSACTIONS` trade-off. One
   error is kept atomic by preflight, but only in the absence of
   concurrent writers: a non-`DETACH` `DELETE` of nodes checks every
   target's degree *before the first batch commits*, so
   `DeleteConnectedNode` fails the statement with nothing written. The
   preflight reads the phase-1 snapshot; a concurrent relationship
   create committed after it can still surface `DeleteConnectedNode`
   mid-run, and then earlier batches stay committed like any other
   mid-statement failure. A non-`DETACH` `DELETE` mixing node and
   relationship targets is not eligible at all (the preflight cannot
   ignore the statement's own relationship targets), and a constraint
   created after phase 1 fails the statement at the next batch.

5. **Observability.** The returned `QueryResult` carries the summed
   `QueryStats` over all batches and no rows (the eligible shapes have
   no `RETURN`), identical in shape to the unbatched result. EXPLAIN
   output is unchanged; batching is an execution strategy, not a plan
   rewrite.

## Alternatives considered

- **Stream rows through the existing pipeline without materializing.**
  Bounds the row buffer but not the transaction: the redo still
  accumulates in one scratch and still lands as one WAL record, so the
  64 MiB payload limit and the memory blowup remain. Rejected — the
  transaction boundary is the actual problem.
- **Rewrite the statement to `CALL { } IN TRANSACTIONS` internally.**
  Reuses existing machinery, but that clause chunks *rows* after they
  are fully produced, keeps one outer transaction (its batches are
  scratch snapshots, not WAL commits), and cannot deduplicate the
  cross-product `DELETE` target space. Rejected — it neither bounds the
  WAL record nor the target memory.
- **Split the one giant WAL record at commit time.** Keeps statement
  atomicity, but the scratch itself (per-op overlay entries) is the
  memory peak, and conflict detection/apply assume one commit_ts per
  record; framing a record across segments complicates replay for no
  semantic gain. Rejected.
- **Batch everything unconditionally.** Also batching explicit
  transactions or constrained catalogs would change atomicity
  guarantees users rely on. Rejected — the conservative gate is the
  point.
- **Raise the 64 MiB WAL payload limit.** Treats the symptom; a large
  enough bulk write always exists. Rejected.

## Consequences

- The listed global bulk shapes complete with memory bounded by the
  batch work budget plus the target id vectors, regardless of match
  size; the 417×417 cross-product create and the 174k-entity detach
  delete both run to completion.
- Durability granularity changes for eligible statements only: a crash
  mid-statement can leave a prefix of batches durable. This mirrors
  `CALL { } IN TRANSACTIONS` and is documented on the CLI/rust spec
  pages.
- Batched statements issue several group commits (one fsync per batch
  under `DurabilityMode::Full`); the batch work budget (25 000
  estimated touched entities) keeps that overhead small, and statements
  under the threshold are untouched.
- Explicit transactions, constrained catalogs, `WHERE`-carrying matches,
  aggregating `WITH`s, and every shape not enumerated above are byte-for-
  byte on the old path; the conservative gate only ever *shrinks* the
  batched set, so future tightenings are not breaking changes.
- The TCK corpus is unaffected: no TCK scenario exercises stores large
  enough to cross the batching threshold.
