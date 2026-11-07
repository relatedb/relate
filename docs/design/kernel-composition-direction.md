# Kernel composition — design direction

Status: design note, not an ADR. No decision is taken here; the direction is
unproven and the open questions are listed in §5. An ADR is minted when the
work is taken up and something is actually decided.

Date: 2026-11-07.

**Question.** Can the contracted traversal kernels become compound operators —
fusing several operations and combining with regular operators and with each
other — instead of each kernel owning a whole `MATCH` clause?

**Answer in one line.** Kernel → regular-operator handoff already exists, and
is the clause pipeline's ordinary contract; only the sinkable-projection
class is gated out. Kernel → kernel and segment splitting do not exist at all.
The smallest useful step is to stop excluding sinkable projections, not to
build a new operator protocol.

## 1. What exists today

**The ladder is first-takes-all.** `run_match_operator_ladder`
(`relate/relate-executor/src/exec/match_clause.rs:65-141`) tries the frontier
family (`:81`), the batch chain (`:100`), the chain/hub-star kernels (`:109`),
then the per-row walker (`:128`). The first operator that takes the clause
returns (`:91`, `:123`, `:126`); no rung hands a partial result to another.
The two rungs below the frontier share one admission gate, computed once
(`:98-99`): no cap except a plain one, and **no sink projection**
(`projection.is_none()`). The kernel rung is gated again on the batch chain
declining (`:108`).

**The matcher is all-or-nothing.** `matcher::compile`
(`exec/kernels/matcher.rs:135-149`) maps the whole pattern list to one
contract: one pattern → chain of 2..=4 fixed hops (`:51`, `:312-358`), two
patterns → hub-star (`:372-512`), anything else declines. It also requires an
empty driving row and `DifferentRelationships` (`:141`). There is no notion
of a segment inside a pattern or of a clause with a compilable part and an
uncompilable part.

**Each kernel compiles the whole clause.** A `Chain` carries the scan
segment's target plus every hop (`matcher.rs:95-101`); `chain::run`
(`exec/kernels/chain.rs:128-147`) walks all hops before emitting.
`HubStar` carries both sides (`matcher.rs:104-125`); `hub_star::run`
(`exec/kernels/hub_star.rs:83`) enumerates hubs and pairs the sides.

**The output contract is plain rows.** `KernelOutput::Rows(Vec<Row>)`
(`exec/kernels/mod.rs:173-177`), materialized at the breaker through
`Chunk::from_columns(...).to_rows()` (`chain.rs:287-302`; `hub_star.rs:198`).
`Row` is the batch substrate's declared boundary representation (ADR-0468).

**The consumer seam already works for everything the sink does not claim.**
The ladder's rows are returned to the dispatcher, which continues with the
next clause (`exec/dispatch.rs:360-371`). So a kernel-served `MATCH` feeding
an aggregate, an eager `SKIP`/`LIMIT` page, or a `WITH` filter already hands
rows to a regular operator. That is composition in the weakest sense, and it
is today's contract — no new machinery.

**What is fused instead of composed.** The frontier family decodes the
statement's *suffix* and answers one consumer shape inside the kernel
(`decode_consumer`, `exec/kernels/frontier_run.rs:93-97`), running the shared
projection tail `reduce_row` per emitted row (`:110-116`, `:152`,
`:493-508`). That is N (pattern families) × M (consumer shapes) fused
kernels — the shape the composition idea exists to avoid.

**The rider is a consumer pushed in, not an output protocol.** `Rider`
(`exec/rider.rs:215-218`) carries a cap or a probe *into* a walk; `PathSink`
(`:229-248`) is the classic walk's sink. `frontier_run` is the plug-in wiring
one family to one consumer inside one clause. It composes a walk with its
consumer, not two operators.

## 2. What "composition" could mean

| Sense | Today | Seam it would attach to | Cost |
| --- | --- | --- | --- |
| **Kernel → regular operator**: kernel emits rows, a projection/aggregate/distinct operator consumes them | Already the pipeline contract for non-sink consumers. Excluded only for the sinkable-projection class by `match_clause.rs:98-99`. | The ladder's `Vec<Row>` return plus the dispatch loop (`dispatch.rs:360-371`). | Small: an admission-gate change plus selection and differential tests. |
| **Segment splitting**: one clause split into a kernel-served prefix and a walker- or kernel-served remainder | Absent. `match_paths` (`match_clause.rs:263-409`) continues across *patterns* inside the walker, but takes whole patterns and is walker-internal. | `match_paths`' per-pattern continuation is the closest shape; nothing exposes it to a kernel. | Large: a partial-output contract, a lineage carrier, segmentation. |
| **Kernel → kernel**: two families over different patterns of one clause, joined on shared variables | Absent. `compile` returns one contract for the whole list (`matcher.rs:144-148`). | `Contract` / `run` (`kernels/mod.rs:311-353`). | Large: a contract vocabulary for a join and a plan for the shared-variable reconciliation. |

The original compound-operator idea is (2) and (3). (1) is the cheap half and
carries most of the near-term value.

## 3. What blocks composition today

1. **Ownership.** The ladder returns on first engagement (`match_clause.rs:91`,
   `:123`, `:126`). There is no protocol by which one operator yields a
   finished prefix and the next takes over.
2. **All-or-nothing proofs.** `matcher::compile` is defined over the whole
   pattern list; the cost gate `engages` (`kernels/mod.rs:440-518`) prices one
   whole contract. Nothing prices a cut or a composition.
3. **No partial-output contract.** Kernels emit only complete bindings, at the
   breaker. The columnar `Chunk` is clause-internal (ADR-0468 scopes it to
   clause interiors and keeps `Row` at the boundary); no interface streams a
   segment's frontier to another operator.
4. **Rel-lineage carriage.** The chain's relationship-uniqueness rule is an
   inline scan of at most four rel ids held in the row (`chain.rs:22-24`); the
   hub-star adds cross-side disjointness. A second segment must inherit that
   lineage, and `Row` has no carrier for it — `Matched`/`UsedRels` are
   walker-internal (`rider.rs:38`).
5. **Answer identity.** Fail-through is clause-granular and the walker is the
   oracle (ADR-0473, ADR-0478). A partial contract needs a proof that a
   segment's rows are exactly the walker's rows for that prefix: rel
   uniqueness, `OPTIONAL` null binding, and error order all cross the cut.
   ADR-0478 Decision 3 is the standing evidence that a plausible-looking
   dedup changes answers.
6. **Sink semantics.** A following non-aggregate `RETURN`/`WITH` is pulled
   into the match sink (`sinkable_projection`, `exec/project.rs:160-211`) and
   the dispatcher then assumes the sink applied it (`RowSource::SinkProjected`,
   `dispatch.rs:373-374`; `project.rs:250-261`). A kernel that hands rows on
   must either run that projection itself (still fusion) or make the
   dispatcher take the ordinary projection path.
7. **Observation and selection tests.** Engagement is counted per clause, not
   per stage (`kernels/mod.rs:520-545`, ADR-0519). No shape→rung table exists
   for compositions, and none may ship without one.

## 4. Minimal first step

**Lift the sink-projection gate for the kernel rung only, and let the ordinary
projection operator consume the kernel's rows.**

Concretely:

- `chain_eligible` (`match_clause.rs:98-99`) keeps its cap condition and drops
  `projection.is_none()` for the chain/hub-star rung. The batch chain keeps
  its gate: its emit is scan-bounded and the sink is its own consumer.
- The dispatcher sets `rows_source = RowSource::Bindings` when the returned
  operator is `Kernel` and a projection rides, so the projection clause
  evaluates normally (`project.rs:387`'s non-preprojected branch, then the
  same sort/page/`DISTINCT`/extraction stages).
- Pin it: a shape→rung table (sink projection present → kernel for the chain
  and hub-star shapes; aggregate and eager page → unchanged) plus a
  differential suite running the same statement through the sink path
  (kernels stood down) and the ordinary projection path, asserting identical
  rows order-insensitively.

Why this first: it needs no new operator protocol, no partial-output
contract, and no lineage carrier. It exercises the one handoff that already
exists, and it admits the standalone `MATCH ... RETURN <expr>` class that the
gate alone excludes.

Caveats, stated plainly:

- The value is unproven until the excluded shapes are counted. The pokec-style
  `MATCH ... RETURN x LIMIT n` shape is *already* admitted: the eager page
  makes `sinkable_projection` decline, so the cap rides the kernel (ADR-0524).
  The remaining class is the plain projection with no eager page — including
  bare `MATCH ... RETURN <expr>`, `DISTINCT` without a cap, and `ORDER BY`
  without a limit. `DISTINCT` or `ORDER BY` combined with a cap stays out
  because that cap is not plain and the gate keeps its cap condition. Count
  the class before committing to this as the first step.
- Row order is the kernel's own (ADR-0473 Consequences), so the projection
  operator must be order-insensitive. The tests above are the check.
- Error timing may differ: the sink evaluates projection items per row during
  the walk; the ordinary path evaluates after the clause's rows are
  collected. Whether CIP-27 makes the sink's timing contractual is an open
  question (§5), not something this note assumes.
- The kernel materializes complete named bindings before the projection runs;
  the sink projects as it lands. The memory envelope is the one the kernels
  already have for aggregate and eager-page consumers, but it should be
  measured for the newly admitted class.

A deduplicating chain variant (per-level dedup for endpoint-only consumers) is
the other candidate the ticket names. It is *not* a smaller step: per-level
dedup changes answers unless the consumer is proven insensitive (ADR-0478
Decision 3 and its alternatives), so it needs the answer-shape proof and a
second kernel variant — the "dedicated implementations and a switch" shape,
not a gate change.

## 5. Settled vs open

Settled by existing ADRs:

- `Row` is the boundary representation; kernels may materialize rows at the
  breaker (ADR-0468).
- A specialized operator may take a cap when it can honor it count-exactly
  (ADR-0524).
- Fail-through and answer identity are non-negotiable; the walker is the
  oracle (ADR-0473, ADR-0478).
- Any consumer-shape proof follows the frontier's answer-shape gate model
  (ADR-0478 Decision 2).
- A diagnostic composition switch would live under the same rules as the
  pattern-engine switch: diagnostic-only, never Cypher syntax, removed with
  the engines it selects among (ADR-0519).
- Per-level dedup is answer-changing without a consumer proof (ADR-0478
  Decision 3 and alternatives).

Open — needs agreement, and an ADR when taken up:

- Which sense is the goal: (1) only, or (1) then (2)/(3), and in what order.
- The partial-output contract for a segment: rows, rel lineage, walk-start
  fact, `OPTIONAL`/error semantics.
- Sink projection: hand rows to the ordinary projection operator, or run the
  projection tail inside the kernel's breaker.
- Whether a cut is planned (a segmentation pass) or opportunistic (each rung
  takes what its proofs admit, in order).
- The cost model for a cut: per-segment estimates, and when a split pays.
- Whether error timing is part of the sink contract (CIP-27) or an
  implementation choice.
- The dedup stage: what consumer-shape proof makes it exact, and whether it is
  a chain variant selected by a switch.
- Engagement observability: per-stage counters, since composition multiplies
  the number of stages a clause can run.

## 6. References

- ADR-0473 — contracted traversal kernels (the matcher families and the
  fail-through discipline).
- ADR-0468 — structure-of-arrays batch substrate (`Row` as the boundary;
  clause interiors only).
- ADR-0478 — the frontier-NFA QPP kernel (the answer-shape gate; Decision 3's
  answer-identity lesson; Decision 4's order contract).
- ADR-0519 — the diagnostic pattern-engine switch (the switch discipline).
- ADR-0524 — the plain limit cap riding the contracted kernels (the precedent
  for a consumer riding a kernel exactly).
