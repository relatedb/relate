---
type: adr
area: engine
claim: 4ab0ff08fe45a579807f739e6225c6f7fc6aa71b
date: 2026-11-21
deciders: Relate maintainers
name: 0641-batched-generator-seeded-tails
status: proposed
tags:
  - execution
  - streaming
title: A generator seeding a per-row-independent tail runs batch-major inside one transaction, never holding the tail's cross product
REFERENCES:
  - "[[adr/0013-tuple-mvcc]]"
  - "[[adr/0016-write-overlay]]"
  - "[[adr/0209-batched-bulk-writes]]"
  - "[[adr/0463-expression-memory-charge]]"
  - "[[adr/0468-consumer-dispatch]]"
  - "[[adr/0555-stream-rows-never-accumulate]]"
---

# ADR-0641: A generator seeding a per-row-independent tail runs batch-major

## Status

Proposed — lands with its implementation.

## Context

ADR-0555 made the operator boundary a pull stream or a fold, and recorded
one residual: the inter-clause boundary (`run_clauses: Vec<Row>`) stays a
whole vector, with per-clause consumer dispatch as the working pattern.
That residual has one shape that violates ADR-0555's spirit without
violating its letter: **a generator clause seeding a tail whose rows are
per-row independent** — the canonical case being the bulk-load statement
`LOAD CSV … AS row MATCH (a:User {id: row.id}) … CREATE …`.

There, no single clause holds an unbounded container: the generator
streams its records (#568), the MATCH walker emits per driving row, the
write arm passes rows through 1:1. But the boundary between them does:
each composed row *carries* the generator's payload (a ~400 B CSV record
across the pokec load's 656 k relationships), and ADR-0463's emission
charge correctly bills rows × width for the held boundary — so the
default 256 MiB per-allocation cap refuses the statement, and with the
cap lifted the load peaks at 13.1 GiB RSS for a ~490 MB store. The
charge is working as designed; the container it prices is the residual.

The statement's own transaction state is a different container: the redo
a write statement accumulates before its one commit is proportional to
its writes by construction, is charged through the workspace budget, and
is bounded at the WAL boundary by the per-record payload cap
(ADR-0086) — whose refusal already names the explicit bounded form,
`CALL { … } IN TRANSACTIONS`. That state is not this ADR's problem to
fix; batching the *tail's rows* is.

## Decision

When a statement's **first clause is a record generator** (`LOAD CSV`)
and the **tail is per-row independent**, the dispatcher runs the tail
**batch-major**: the generator produces bounded batches of driving rows
(≤ 1024, ADR-0555's batch bound), and the remaining clauses run once per
batch over that batch's rows, through the existing nested-clause runner,
**on the statement's own scratch**. No inter-clause container in the
tail ever exceeds one batch. The redo accumulates in the scratch as the
transaction's own state and commits **once, with the statement** — there
is no implicit per-batch commit. Batching here is an execution-shape
decision inside one transaction, not a durability decision; the
statement-authored per-batch commit form stays `CALL { … } IN
TRANSACTIONS` (ADR-0210), and nothing runs it implicitly.

**Eligibility** (all must hold, decided once at the top of the clause
loop, before any arm runs):

1. The first clause is `LOAD CSV` (the one generator whose source is
   external and unbounded; `UNWIND`'s list source is already in memory,
   and a `CALL` generator takes the resolution's own path).
2. The tail is per-row independent: no aggregation, `collect`,
   unordered `DISTINCT`, sort, or subquery in the remaining clauses —
   an aggregation in the tail would aggregate per batch, not over the
   statement, so the shape is wrong, not merely unaesthetic. A terminal
   `RETURN` of the batch's rows is *ineligible* (the page is the whole
   point of holding; it answers through the ordinary path).
3. The tail's reads precede its writes and the write patterns are
   disjoint from the read patterns (ADR-0209's independence condition,
   spelled for the clause vocabulary): a later batch's MATCH must not
   see an earlier batch's writes — they share one scratch overlay.
4. The tail's write effects, if any, are per-row (the write arms' own
   1:1 pass-through shape) — a write whose effect spans rows (none
   exist today) would break the batch equivalence. The admission reuses
   ADR-0209's canonical per-row-independence predicate, whose property
   expressions may reference matched variables only: a `CREATE` whose
   property is sourced from the generator's own record (e.g. a
   relationship weight read from the CSV column) is declined and falls
   to the ordinary path. That is deliberate conservatism — widening the
   predicate to the generator's row, which is per-row constant, is a
   recorded follow-up, not this ADR's scope.

**Semantics.** Batching changes the container shape, nothing else: the
statement stays one transaction with its ordinary atomicity — final
state, summed write statistics, and terminal result equal the unbatched
run. No implicit sub-transactions are needed to make that safe: the
statement's in-flight writes live in the write overlay over a pinned
snapshot (ADR-0016), and the store's version tracking (ADR-0013 tuple
MVCC, the optimistic conflict check) carries isolation — batches are
invisible scheduling units, never transaction boundaries. The
transaction's redo grows with the writes as any write statement's does;
a statement whose single commit would cross the WAL payload cap refuses
with the capacity error that already names the remedy (`CALL { … } IN
TRANSACTIONS OF n ROWS`, or client-side batches) — the explicit,
statement-visible form, never an implicit one.

**Charging.** The batch container is ADR-0555's bounded boundary itself —
at most 1024 rows, the batch bound — not an accumulation, so it is not a
charged retention: the charge exists to price containers that grow with
the input, and this one does not. The tail's own inter-clause containers
are charged as before (ADR-0463), so a pathological *tail* still refuses.
One consequence is recorded rather than solved here: a single pathological
generator record (a multi-megabyte CSV row) rides the batch uncharged, so
the emission cap does not refuse it — bounded by the CSV source policy's
own file cap. Pricing the batch through the same `retained_rows_bytes`
protocol the materializing form uses is a recorded follow-up, not this
ADR's scope. The redo is charged through the workspace budget exactly as
today.

**The recorded residual is narrowed, not erased.** `run_clauses`'s
`Vec<Row>` boundary remains the interface for every non-batched
statement; ADR-0641 carves the generator-seeded, per-row-independent
tail out of it. A general chunked boundary (ADR-0468's substrate at the
clause level) stays future work — this ADR deliberately does not
restructure the clause loop's contract, only adds one eligible
batch-major entry beside `stats_short_circuit`.

## Consequences

- The pokec-class bulk load answers under the default
  256 MiB emission cap; peak RSS during the rels phase scales with the
  batch budget plus the transaction's redo, not the record count.
- A load whose tail is ineligible (an aggregation over the load, an
  ordered page) keeps today's semantics and today's charge — the
  refusal, when it comes, is the correct signal for that shape.
- A load whose *commit* is too large for the WAL boundary keeps the
  ordinary refusal naming `CALL { … } IN TRANSACTIONS` — the
  maintainer's lever, not a silent re-plan. (Raising the payload cap
  itself, and making it configurable, is recorded separately.)
- An in-flight transaction whose state outgrows the memory segment is a
  capacity question with two escapes, neither implicit: the
  statement-authored `CALL { … } IN TRANSACTIONS` today, and — as the
  recorded future direction, not this ADR's work — spilling the write
  overlay's in-flight data to disk, which the version tracking already
  permits because in-flight state is a private overlay over an
  immutable snapshot, not a shared structure.
- The eligibility check is a new classification at the clause-loop top;
  it is spelled once, conservatively (fail to the ordinary path), and
  pinned by tests both ways.
- No new commit machinery: no per-batch sink, no reuse of the ADR-0210
  batch-commit path, no change to `relate::commit`. The driver lives
  entirely in the executor's clause loop.
- ADR-0555's residual list loses its one input-proportional case; the
  remaining residuals (the MATCH fallback's charged landing, the GPM
  enumeration) are unchanged.
