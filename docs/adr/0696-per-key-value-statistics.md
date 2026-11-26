---
type: adr
area: engine
claim: 1e9f6e7c03a8d7a3b0de3901b5879cce14ddd7a0
name: 0696-per-key-value-statistics
status: proposed
tags:
  - executor
  - planner
  - perf
  - memory
title: Per-key value statistics, built lazily per label, with provenance
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0172-incremental-constraint-validation]]"
  - "[[adr/0355-fxhash-core-maps-and-interned-domain-reads]]"
  - "[[adr/0497-catalogue-corpus-alignment]]"
  - "[[adr/0508-planner-stats-consult-memo]]"
  - "[[adr/0614-equality-match-lazy-postings-serve]]"
  - "[[adr/0686-pair-postings-packed-bucket]]"
---

# 0696: Per-key value statistics, built lazily per label, with provenance

## Context

The planner prices a pattern from label counts, relationship-type counts,
degree histograms, and two value-keyed consults: `equality_rows` and
`range_rows`, both answered from an index and `None` without one. Nothing
records how many distinct values a `(label, key)` holds, how they are
distributed, or their range. An unindexed equality (`City.country = $c`)
or range (`Person.age BETWEEN`) therefore falls to a fixed rank, and the
lazy equality pair of [ADR-0614](./0614-equality-match-lazy-postings-serve.md) sizes its table from the label's row
count, not from the number of values.

Measured on a synthetic graph (release build, 13 properties per node, two
labels), a scan that reads every live node once and updates a sketch per
`(label, key)` costs, per million nodes: 6.5 s when every value is hashed,
about 0.33 s when long strings and lists are gated out and the sketches
are 1 KB. The gated scan is about 4% of the cost of building the graph.
The sketches cost about 1.6 KB per key, constant in node count. The scan
holds the node store's read lock for its whole walk.

## Decision

1. **Statistics per `(node label, key)`**, over the admitted values of
   that key on the label's live nodes: the count of admitted values, the
   minimum and maximum (numbers, booleans and the temporal kinds,
   within one family; a key mixing families has none; strings, points and
   references have none), the sum (numeric
   keys, so a mean is `sum / count`, defined when the sum is finite), an approximate distinct count
   (HyperLogLog, 1 KB of registers, about 3% error) and a top-16
   heavy-hitter summary (space-saving; counts are upper bounds, and a
   value is listed only when its guaranteed lower bound, the count minus
   the error recorded when it was admitted, exceeds 1/32 of the rows, so
   a key with no truly heavy value lists none and the estimate falls back
   to rows over distinct values). Relationship keys are out of scope.
2. **A type gate decides admission, before any hashing.** Admitted:
   integers, booleans, non-NaN floats, temporal values, points, node and
   relationship references, and strings of at most 64 bytes. Declined:
   longer strings, lists, maps and vectors. A declined value costs the
   predicate and a per-key counter, so a key that is mostly declined is
   known to be so and takes no sketch.
3. **Built lazily, per label, never persisted.** The first consult for a
   label requests a build that scans its live nodes once and builds every
   admitted key of that label, in bounded chunks that release the node
   store's lock between chunks. Every node write counts against each of
   its labels; once a built label has taken more than one thirty-second
   of its rows in writes (at least 64) it turns stale and the next
   consult requests a rebuild, so a label under a steady write stream does
   not rescan on every write. Until a label is built, while it
   is stale and while it is being built, a consult returns `None` at once
   and never waits, which every planner ladder already reads as
   unmeasured. A built label's consult is one map lookup. The consult
   only records a build request. The engine's one maintenance seam serves
   the requests: the `relate` connection calls the graph's pump
   (`Graph::build_pending_key_stats`) when a statement completes, on every
   statement path (an auto-commit read or write, a statement inside an
   explicit transaction, the streaming read), after the statement's own
   snapshot pin and guards are released and only if the statement
   succeeded, for at most one requested label, so a statement inherits at
   most one label's scan and later statements serve the rest. The scan
   stops at the next chunk boundary when the statement's cancel token
   fires, and its label returns to the queue. The read executor never runs
   a build. A build that saw more writes than the stale threshold while it
   scanned still publishes, and its write count restarts, so a label
   written faster than it can be scanned is rebuilt once per threshold of
   writes and never once per read. The any-label bucket is unsupported (no
   write ages it): a consult for it is unmeasured and requests nothing.
   Nothing is computed at load or open and nothing is written to a
   checkpoint.
4. **A byte budget caps the sketches, with a priority.** The statistics
   hold at most a fixed number of bytes (a constant, about 5 MB), charged
   including the payload of every stored string. A label's scan holds at
   most 2,048 key accumulators (about 1 KB each, transient): the first
   keys seen keep theirs for the whole scan and a later key is refused for
   the whole scan, so a published key was read from its first value to its
   last. Keys of structured types (numbers, booleans, temporals, points,
   references) outrank string keys at publish; when the budget is full a
   structured key evicts the published entry of a string key, and an
   evicted or never-admitted key answers `None`. The statistics are best effort, and
   the budget lives outside the [ADR-0042](./0042-graph-memory-limit.md) logical charge, as the lazy pairs do.
5. **Every answer carries its provenance.** `UpperBound` (a declared
   index's posting length, or a built pair: postings are a superset of
   the live values, so counts are upper bounds), `Sketched` (this ADR's
   sketches) and `Exact`, ordered by trust. `Exact` is reserved: it has
   no producer yet and is the answer a built lazy pair with no write
   since its build will give, when a later slice reads that pair for its
   distinct count and largest buckets. A declared index's answer is never
   reported `Exact`, because its postings keep superseded versions until
   the next prune. The equality estimate tries a declared index first
   (`UpperBound`), then the sketches, then `None`.
6. **Planner surface**, in `PatternStats`, both with a default `None`:
   `key_stats(label, property) -> Option<Arc<KeyStats>>` (count, minimum,
   maximum, sum, distinct, top-16, provenance) and
   `equality_estimate(label, property, value) -> Option<RowEstimate>`
   (the heavy-hitter summary's count for a heavy value, an upper bound,
   `rows / distinct` otherwise, with provenance). `equality_rows` keeps its contract (an
   index hit count, `None` without an index). Both new calls go through
   the per-statement memo of [ADR-0508](./0508-planner-stats-consult-memo.md).
7. **The lazy equality pair presizes from the distinct count** when the
   key's statistics are built. After the build's first
   `PRESIZE_PREFIX` candidates the busiest table reserves `distinct`
   plus a 5% margin (capped at the candidate count) minus the buckets it
   holds, whatever the prefix looked like; without published statistics
   the leading-sample estimate of
   [ADR-0686](./0686-pair-postings-packed-bucket.md) stays. The build reads the
   statistics without requesting them (a pair is built once per process,
   so a request made for it would scan the label to benefit a build that
   has already happened) and never waits for a statistics build. Only the
   node pairs use it: relationship keys have no statistics. Where the
   sample already works (a representative unique prefix) the table is the
   same; the statistics help when the first candidates are not
   representative (sorted or clustered ids), in either direction: a
   one-valued prefix over unique ids stops the table doubling through the
   tail, and a unique prefix over a handful of values stops it reserving
   for the whole label.
8. **`db.stats.retrieve('GRAPH COUNTS')` serves the statistics** as an
   added `keyStatistics` list in `data`, beside constraints and indexes:
   one entry per published `(label, property)` with its counts, distinct
   estimate, range, sum and mean, heavy values and provenance, ordered by
   label then property, and listing only keys whose label is built (a read
   never triggers a build or records a request). The procedure's signature
   is unchanged; only its payload grew, and the existing entries are
   untouched.
9. **Later slice, not implemented here: case-folded strings.** A
   `tolower(x) = tolower($p)` equality would be priced from the raw
   distinct count as an upper bound on the folded distinct count, flagged
   `Sketched`; neither that proxy nor an exact folded sketch exists in this
   change.

## Alternatives considered

- **Compute every key at load or open.** About 4% of load time with the
  gate, but it makes every load pay for statistics most graphs never
  consult. Rejected for lazy per label, the trigger the equality pair
  already uses.
- **Persist the statistics in the checkpoint.** Avoids the first-consult
  scan, at the cost of a format change and a staleness rule. Rejected for
  now; recomputation is cheap and the dirty mark already needs a rebuild.
- **Hash every value, no gate.** 12 times slower on a graph with text and
  embedding properties for statistics nobody consults. Rejected.
- **A histogram per numeric key.** More precise ranges; deferred. Minimum
  and maximum under a uniform assumption price a range now.
- **Counting sketches that support deletion.** Heavier per key and still
  approximate. Rejected for the dirty-mark rebuild.

## Consequences

- This change measures and forwards but does not yet change any plan:
  nothing in the planner consumes `key_stats` or `equality_estimate`, and
  no range estimate is built from the minimum and maximum. The consumer
  lands in the planner's ordering ladder in a separate change; until then
  the build seam runs only for requests the planner makes, which today it
  makes none of. Once consumed, an unindexed equality on an admitted key
  gets an estimate with a stated provenance where it fell to a rank
  before.
- The equality pair's build benefits only when a consult had the key's
  statistics built before the pair was: it never asks for a build or waits
  for one, so a graph whose planner never consults sizes its pairs exactly
  as before. `GRAPH COUNTS` lists the built set and is a pure read.
- The first consult on a label pays one chunked scan (about 0.3 s per
  million nodes on the measured shape); later consults are memo hits until
  a write dirties the label.
- Up to the budget in extra memory outside the logical charge; a graph
  with few keys pays kilobytes.
- Distinct counts are within a few percent; top-k counts are upper bounds
  and list a value only when it is clearly heavy (roughly above 10% of the rows, where the guaranteed lower bound clears 1/32).
- A key whose values are mostly declined, or evicted under the budget,
  answers `None`; planning is never worse than today.
- The collector, its budget and its dirty mark are the one place that
  owns statistics state; the pair and the planner read it through the
  accessor and never reach the sketches.
