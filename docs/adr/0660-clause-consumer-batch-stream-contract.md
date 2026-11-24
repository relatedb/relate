---
type: adr
area: engine
claim: a7d62dc38689bac6033c5bc5b5d939dddde31fe5
date: 2026-11-22
deciders: Relate maintainers
name: 0660-clause-consumer-batch-stream-contract
status: proposed
tags:
  - execution
  - streaming
title: The inter-clause boundary is one batch stream, and every clause arm declares its disposition
REFERENCES:
  - "[[adr/0243-query-latency-suite]]"
  - "[[adr/0463-expression-allocation-budget]]"
  - "[[adr/0468-soa-batch-substrate]]"
  - "[[adr/0555-operators-stream-rows-never-accumulate]]"
  - "[[adr/0641-batched-generator-seeded-tails]]"
---

# 0660: The inter-clause boundary is one batch stream

## Context

[ADR-0555](./0555-operators-stream-rows-never-accumulate.md) made the
operator boundary a pull stream of rows or of bounded batches, and named
the remaining residual: the inter-clause boundary is still a whole row
vector, with a second, parked-feed slot beside it for a generator fused
into its consumer. [ADR-0641](./0641-batched-generator-seeded-tails.md)
carved one eligible generator-seeded tail out of that vector and recorded
the general case as future work.

The residual costs what the per-pair fusions never reach: each clause arm
re-derives its input shape, the dispatcher carries two channels (the held
vector and the parked-feed slot), and nothing states which clause may
consume rows as they arrive. A statement whose terminal projection pages,
filters, or folds still hands every clause the whole input, so a bounded
answer materializes its entire population. PROFILE row counts and error
ordering therefore differ by arm, with no recorded rule.

The missing piece is not another fusion. It is a consumer contract: one
input type every clause arm receives, and one declared disposition per
arm.

## Decision

**The seam.** The inter-clause boundary is one type, `RowStream<'a>` —
the only row-bearing input a clause arm receives. It replaces the held row
vector and the one-slot parked-feed channel: a generator spine
(`StreamedFeed`) becomes a source of the stream, parked in the stream slot
itself, never a second channel beside it. The projection's `RowFeed` is
reduced to its two input shapes — the stream, and the aggregation a
producer already folded — so a projection receives no row vector and no
parked feed. The consumer never branches on the source: a held container
and a generator clause are the same stream to it.

A stream yields **bounded batches** — `RowBatch`, at most `BATCH_ROWS`
(1024) rows, [ADR-0555](./0555-operators-stream-rows-never-accumulate.md)'s
batch bound — through:

- `next_batch`: the next batch, or exhaustion; an error ends the stream.
- `stop`: the consumer's early exit; the producer generates nothing further.
- `probe`: the rows already in hand, for the shape and warm-up decisions
  that must read a row before the walk starts (empty when generated).

**The transformer contract.** Every clause arm is a transformer: it takes
the one `RowStream` and yields its own `RowStream`, never a held vector of
rows. It pulls only as it emits — one input batch in, its disposition's
output batches out — and stops pulling when its consumer stops it. A
producer fused into a consumer (an `UNWIND`, a `LOAD CSV`, a `CALL { … }`
body, or a `MATCH` a projection folds) is a source of that same stream,
never a second channel beside it. An arm whose answer needs its whole input
still takes and yields the stream: it declares the state it holds (the
`retain` disposition), and emits its own batch stream once it can answer.

**Dispositions.** Every arm of the clause dispatcher declares exactly one.
Where an arm's shape carries more than one disposition, the most retaining
one wins: `retain` over `page` over `pass`.

| arm | disposition | meaning |
| --- | --- | --- |
| `Create`, `Merge`, `Set`, `Remove`, `Foreach`, `Use` | `pass` | transforms each row as it arrives — a write clause passes the row through with its statistics, `Foreach` applies its side effects and keeps it — and emits at most one row per input row |
| `With`, `Return` with no aggregation, `OrderBy`, `SKIP`, `LIMIT` or `DISTINCT` | `pass` | each item is evaluated per row and the row emitted as its batch arrives; a post-projection `Where` filters per row |
| `With`, `Return` with `SKIP`/`LIMIT`/`DISTINCT`, or a bounded `OrderBy` with a `LIMIT` | `page` | streams batches, applies the page directly, and retains only the counter, the distinct seen-set, or the top-k heap |
| `Match`, `Call`, `Unwind`, `LoadCsv`, `CallSubquery` without `IN TRANSACTIONS` | `expand` | streams its input and yields zero or more rows per input row from its own walk |
| `Delete` | the whole-clause hold of [ADR-0676](./0676-delete-holds-its-input-whole.md) | holds its driving input and resolves every row's targets before the first node delete — a row's relationship delete may be the one detaching a node another row deletes, an invariant no batch boundary may split |
| `With`, `Return` with an aggregation, an unlimited `OrderBy`, or a star aggregation; `Conditional`; `CallSubquery` with `IN TRANSACTIONS` | `retain` | a declared breaker: holds only its declared state, charged ([ADR-0463](./0463-expression-allocation-budget.md)), emits its own batch stream, and answers only after its full drain |
| `Return` | `terminal` | consumes the stream as the part's result as batches arrive, and ends the part |
| `Catalog` | `source` | emits its own bounded catalog rows — cross-joined with the input stream where the command admits rows — and reads nothing from the walk |
| `ShowIndexes`, `Finish` | `terminal` | answers the part directly, with no row walk |
| `CreateIndex`, `DropIndex` | `schema` | no row interaction; the stream passes through untouched |

The parked and held forms of `Unwind`, `LoadCsv`, `Call`, and `CallSubquery`
share the `expand` disposition; parking is a source fact, not a consumer one.

**The projection rules.** A `With`/`Return` with no aggregation and no
`OrderBy` passes the stream through: each item is evaluated per row and
the row emitted at once, and a post-projection `Where` filters per row.
`SKIP`/`LIMIT` apply directly on the stream — the clause stops pulling
once its page is satisfied. `DISTINCT` passes through each item it has not
seen before and retains only its seen-set,
[ADR-0555](./0555-operators-stream-rows-never-accumulate.md)'s charged
distinct breaker; a `SKIP`/`LIMIT` after it applies to the distinct output
and may stop the stream. `RETURN` emits its page as batches arrive, so rows
reach the result in batch order.

**The state-holding transformers.** An **aggregation** (the group table,
and a `collect` or a percentile that retains its operands), an **unlimited
`OrderBy`** (the whole input, sorted), a **star aggregation** (`*` groups
by every carried variable, which needs the rows' shape up front) and a
**join build side** (the input rows it indexes) are transformers too: each
holds only its declared state, charged to the retaining-container budget
([ADR-0463](./0463-expression-allocation-budget.md)), and emits its own
batch stream once it can answer. An `OrderBy` with a `LIMIT` is the bounded
top-k form, not a breaker. A star projection with no aggregation is a
visible-bindings pass-through.

**Stop-early.** `pass`, `page` and `expand` stop with their consumer: the
stop ends the chain, and the producer stores nothing after it. A `page`
clause also stops itself as soon as its page is satisfied. A `retain`
disposition cannot stop on its input — it must drain to answer — but its
downstream may stop after its first output batch. A `terminal` that
answers a whole result drains; a paged terminal stops at its bound, and a
`Return` stops pulling as soon as its last page batch is emitted. A
`source` reads nothing before its own rows are consumed, and `schema`
passes the stop through.

**Error ordering.** Every disposition surfaces a producer's error at the
pull that produced it, in producer order and before any later batch; a
batch already delivered is not retracted, and a stop placed before an
error means the error is never raised. A `retain` disposition drains
first, so every input error precedes its first output row. This is the
specification-visible change [ADR-0555](./0555-operators-stream-rows-never-accumulate.md)
recorded: the same statements raise the same errors unless a bounded
consumer stops before them.

**PROFILE row counts.** Each consumer ticks the rows it emitted, never
its input count: `pass` the rows it transformed, `expand` the rows its own
walk produced, `page` its page, `source` its own rows, `terminal` its
result rows, `schema` none, and `retain` the rows it consumed into its
retained state beside the rows it emits at exhaustion. A parked producer's
facts report the rows it produced up to the drain or the stop, never a
projected population.

## Alternatives considered

- **Keep the vector and fuse each streaming pair by hand.** This is the
  status quo behind the earlier carve-out. Rejected: every arm re-derives
  the input shape, the second feed channel survives, and no rule says
  which arm may stream.
- **A row-at-a-time stream with no batches.** Rejected: the batch
  substrate of [ADR-0468](./0468-soa-batch-substrate.md) needs batch
  boundaries inside a clause, and [ADR-0555](./0555-operators-stream-rows-never-accumulate.md)
  already admits both forms.
- **Push-based callbacks into the consumer.** Rejected: it recreates the
  per-row dynamic dispatch [ADR-0468](./0468-soa-batch-substrate.md)
  removes.
- **Make the breakers stream too (external sort, spill).** Rejected for
  this decision: breakers stay declared and charged
  ([ADR-0463](./0463-expression-allocation-budget.md)); spilling is
  separate storage work, not a clause-boundary contract.

## Related designs

**Morsel-driven execution** (Leis, Boncz, Kemper, Neumann, CIDR 2014)
hands cache-sized morsels to workers at run time, so one scheduler — not a
fixed partition — decides who runs what. **The vectorization argument**
(MonetDB/X100) makes the same unit a cache decision: a batch amortizes
per-row interpretation and keeps the working set cache-resident. This seam
takes the mechanic the two share — a bounded batch is the unit of both
cache and work — and none of the parallel scheduler.

| morsel mechanic | here |
| --- | --- |
| the batch is a cache decision | `BATCH_ROWS` bounds a `RowBatch`; a byte-aware bound that sizes the batch to the cache is a future direction |
| batch × segment is the unit of work | a `pass`/`expand` arm transforms one batch of one segment at a time; a `retain` disposition closes the segment and drains it before it answers |
| offer-time state charging | a `retain` arm charges its declared state ([ADR-0463](./0463-expression-allocation-budget.md)) when the rows are offered, not when it answers |
| one owner per batch | one consumer owns each batch, so its scratch is reused batch to batch |
| batch-boundary cancellation | `stop` lands on a batch boundary; the producer generates and stores nothing after it |
| the pull loop is the dispatcher | `next_batch` is the dispatcher loop; parallelism stays behind the seam as a later concern |

**The ruling.** Management overhead never trumps the bare execution speed
of a query. The mechanics this seam declines — thread-parallel morsel
scheduling, a byte-aware batch bound, and dynamic work stealing — are
future directions behind the seam, not implementation commitments.

**The batch bound.** `BATCH_ROWS = 1024`
(`relate/relate-executor/src/exec/batch_tail.rs`,
[ADR-0641](./0641-batched-generator-seeded-tails.md)) is an unmeasured
initial assumption, not a tuned value. The sweep that measures it is the
query-latency SF0.1 suite ([ADR-0243](./0243-query-latency-suite.md))
plus the `pokec_profile` lane (`docs/datasets.md`).

## Consequences

- One input crosses a clause boundary; the dispatcher no longer carries
  two channels, and a whole row vector stops being an operator interface.
- A new clause arm must declare its disposition in review; an arm with no
  declaration is rejected the way [ADR-0555](./0555-operators-stream-rows-never-accumulate.md)
  already rejects a new unbounded vector.
- A paged statement stops its producer early; an error behind the page is
  not raised — the documented consequence of streaming, not new behaviour.
- Breakers keep their semantics and their
  [ADR-0463](./0463-expression-allocation-budget.md) charge; only the
  container that feeds them changes.
- PROFILE row counts become a fact of the disposition rather than each
  arm's own choice.
- The migration is incremental in [ADR-0555](./0555-operators-stream-rows-never-accumulate.md)'s
  order: terminal projection, then the pass-through and expanding arms.

## Later references

- **ADR-0676** corrects this ADR's disposition table: `Delete`'s disposition
  is the whole-clause hold, not `pass` — its rels-before-nodes delete
  ordering is a whole-clause invariant the write barrier's per-batch split
  broke (the spec corpus's Set scenario). The other write arms keep `pass`.
