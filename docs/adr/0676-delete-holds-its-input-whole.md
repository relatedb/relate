---
type: adr
area: engine
claim: 637ebcee90d60f8d1fb1ef69dea53a4f3b6fea8a
name: 0676-delete-holds-its-input-whole
status: accepted
tags:
  - executor
  - memory
title: DELETE holds its driving input whole; relationship deletes land before node deletes
REFERENCES:
  - "[[adr/0660-clause-consumer-batch-stream-contract]]"
  - "[[adr/0555-operators-stream-rows-never-accumulate]]"
  - "[[adr/0463-expression-allocation-budget]]"
---

# ADR-0676: DELETE holds its driving input whole; relationship deletes land before node deletes

## Context

ADR-0660 made the write clauses `pass` dispositions: `RowStream::write_barrier`
pulls the driving rows a batch at a time and hands each batch to the write
arm, so a clause's writes land per batch as its consumer pulls. For `DELETE`
that split changed a semantic invariant. The arm resolves each row's targets
and deletes them in one pass — relationships first, then nodes — and a row's
relationship delete may be the one detaching a node another row deletes:
`MATCH (n)-[r0]-() MATCH (n)-[r1]-() DELETE r0, n` deletes every relationship
as some row's `r0` before the node delete can fire. Under the barrier's
per-batch call the node delete of an early batch runs while later batches'
relationship deletes have not, and the delete of the still-connected node
fails with `DeleteConnectedNode` — a regression the spec corpus's Set
scenario (`A property mutated in the SET still drives the filter and the
delete`) caught: the whole-clause ordering is not a batching detail but the
clause's contract.

## Decision

1. **`DELETE` holds its driving input whole.** The arm drains its input
   stream and resolves every row's targets in one pass, then deletes:
   relationships across the whole clause first, nodes after. The arm's
   disposition is the hold, not the `pass` ADR-0660's table gave it; the
   table's row is corrected by this ADR (ADR-0660 keeps its Later references
   note).
2. **The hold is bounded and charged like every materializing boundary.**
   `drain` bills the rows it collects on the workspace ledger, and the arm
   answers as a held stream through the shared clause-exit tail — the
   `MATCH` arm's shape, PROFILE included.
3. **The other write arms keep the barrier.** `SET`, `REMOVE`, `MERGE`,
   `CREATE` and `FOREACH` have no cross-row delete ordering: each row's
   write is self-contained, so their `pass` disposition stands.
4. **A streamed `DELETE` remains future work.** Splitting the arm into
   per-batch relationship deletes with node deletes deferred to the input's
   end would preserve the invariant and stream; the deferral machinery (a
   finish hook on the barrier, the pending-id container's charge) is
   recorded as follow-up work on the review-tasks ledger, not decided here.

## Consequences

- `DELETE r, n` shapes answer again exactly as the pre-streaming engine and
  Neo4j do: the side effects are the full relationship set plus the nodes,
  never a `DeleteConnectedNode` refusal.
- A `DELETE` whose driving input outgrows the workspace pool refuses at the
  door like any materializing clause, where the barrier's streamed form
  answered under a one-batch pool. That is the same trade the
  pre-streaming engine made; the streamed form (Decision 4) is the way back.
- The spec corpus's Set scenario passes on this engine again.
