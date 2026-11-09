# ADR-0555: Operators stream rows; unbounded results are never accumulated

Status: Proposed

Claim: 1c6340d02484f9d785398efc754c6234bcdd6506

## Context

The executor runs a statement clause by clause, and the value passed from
one clause to the next is a whole vector of rows. ADR-0468 states the
starting point: clause interiors are row-at-a-time, with the row as the
boundary representation. That is a CPU-cost decision; it said nothing about
how many rows may be alive at once, so the vector became the interface by
default. Every operator that returns a vector of rows holds its entire
output before its consumer sees the first row.

The cost showed up wherever a result is large but its answer is small. A
bounded variable-length path match from a high-degree node yields millions
of path rows and a few hundred thousand distinct endpoints; the vector of
path rows, charged at about a kilobyte each as one retaining container
(ADR-0463), crossed the per-value cap although the aggregation consuming it
held a few dozen bytes. A list generator feeding an aggregate had the same
shape until it was fused with its consumer. The streaming accumulators for
aggregates (ADR-0499 and its successors) removed the aggregate's own state
but not the input it was fed from.

A per-statement memory cap treats retained rows as the engine's own state,
so the shape also decides which queries are refused. Memory that is
proportional to the number of intermediate results, rather than to the
number of distinct answers or groups, is a defect, not a tuning problem.

## Decision

1. **The operator boundary is a stream.** What one operator hands its
   consumer is either a pull stream of rows or a stream of bounded
   batches of rows. A batch holds at most 1024 rows; the default is in the
   low hundreds, sized to the cache, not to the result. A producer whose
   walk is recursive may instead push each completed row into its
   consumer's fold; that is the same contract seen from the other side and
   is equally allowed.
2. **An unbounded result is never accumulated.** No operator may return,
   or hold, a vector whose length grows with the number of intermediate
   rows, unless it is a declared breaker (rule 3).
3. **Breakers are explicit and charged.** Sorting without a limit, a
   retaining aggregate (collect, percentiles), a distinct set, and a hash
   join build side may hold state proportional to their input. Each does
   so deliberately, charges that state to the retaining-container budget
   (ADR-0463), and prefers a bounded form where one exists (a top-k heap
   for ordered limits, a seen-set of keys for distinct instead of retained
   operands).
4. **Small, closed results may stay vectors.** Catalog listings, procedure
   output, and a statement's final bounded page are not intermediate
   results and need no streaming.
5. **New operators follow this rule from the first commit.** A change that
   adds an operator or clause arm returning an unbounded vector of rows is
   rejected in review; existing vector-returning operators migrate
   incrementally, outermost consumers first (aggregation, then projection
   with early exit, then the remaining generators and match rungs).
6. **Peak memory is observable.** The profiling output reports the working
   set of a streamed operator as one batch, not the retained total, so a
   regression to accumulation is visible.

## Alternatives considered

- **Keep the vector and raise the cap.** Hides the defect and moves the
  failure to a larger input. Rejected: memory would still scale with
  intermediate results.
- **Fuse each producer with each consumer by hand.** Works per pair, and
  was the first step for list generators, but multiplies code by the
  number of pairs. Rejected as the general mechanism; kept as a
  specialisation where a pair is hot.
- **A full vectorised, push-based engine rewrite.** Solves the problem
  uniformly but is a rewrite of the executor. Rejected as a prerequisite;
  the stream boundary is introduced operator by operator.
- **Row-at-a-time only, with no batches.** Simplest contract, but gives up
  the batch substrate's per-column speedups (ADR-0468). Rejected: batches
  of bounded size keep both.

## Consequences

- The agent contract gains a code-shape rule enforcing decisions 2 and 5.
- Aggregating consumers become the first streaming sinks for pattern
  matching, so aggregate memory no longer depends on path count.
- Operators with early exit (limits, existence probes) can stop a
  producer without finishing it.
- Charging moves from "the whole output" to "retained state", so refusals
  track distinct answers and groups, not intermediate volume.
- Error ordering can change where a producer previously finished before
  its consumer started; the specification-visible behaviour is unchanged
  because errors still surface for the same statement.
