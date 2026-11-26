---
type: adr
area: engine
claim: df8ed207426f7439e70e3b717e78c8549e02cb8b
name: 0693-two-hop-tally-fold
status: proposed
tags:
  - pattern-matching
  - perf
title: The two-hop tally fold — a same-way two-hop count tallies per middle node
REFERENCES:
  - "[[adr/0473-contracted-traversal-kernels]]"
  - "[[adr/0555-operators-stream-rows-never-accumulate]]"
  - "[[adr/0657-executor-memo-budget-open-setting]]"
---

# ADR-0693: The two-hop tally fold — a same-way two-hop count tallies per middle node

## Context

`MATCH (a)-[:R]->(b)-[:S]->(c) RETURN count(*)` is answered today by the path
aggregate fold, which visits every path: on a 100k-node, 2.4M-edge graph the
count has 58M paths and takes about 7 s (a reproduction of the graph-benchmark
workload; other engines answer it in 5-300 ms). The chain-count kernel
([ADR-0473](./0473-contracted-traversal-kernels.md)) tallies the last hop but
still checks every candidate, lands the earlier hops as one stage across all
starts, and is behind the path fold in the ladder.

## Decision

1. A new count fold, `try_fold_two_hop_tally`, sits ahead of the path fold in
   the default-mode single-hop ladder. It takes exactly: one pattern of two
   directed hops in the same direction, static type lists, default match mode,
   a `RETURN count(*)`/`count(<pattern var>)` with no grouping, ordering or
   paging, one entering row. Everything else declines to the existing rungs.
2. The count factors through the middle node: for a start `a` and an admitted
   first-hop arrival `b`, the completions are the admitted second-hop arrivals
   of `b`. The fold memoises that tally per `b` and adds it once per
   first-hop edge: `O(E1 + E2)` edge visits, not `O(paths)`.
3. Relationship uniqueness is exact: for two hops of the same direction the
   first rel can be a second-hop candidate only when `a == b`; that case is
   tallied explicitly with the first rel skipped and never reads the memo.
4. The per-middle-node tally rides the kernel family's own memo
   ([ADR-0657](./0657-executor-memo-budget-open-setting.md)'s `Memo`: the statement's
   shared memo budget and ceiling, the hit-rate review, the PROFILE memo
   counters), with a short warm-up because the memoised function is a whole
   adjacency walk. A refusal or a bypass never fails the statement: the fold
   tallies the middle node again, at the per-path cost. Beyond the memo the
   fold retains the admitted start candidates and the widest start's
   first-hop buffer; both are charged to the expression budget as retaining
   containers ([ADR-0555](./0555-operators-stream-rows-never-accumulate.md)'s
   rule), so a statement under a tight cap fails with the usual
   expression-memory error where the path fold's constant-memory walk would
   have passed. The first hop's candidates for one start are buffered before
   tallying so no adjacency walk nests inside another.
5. No cost gate: the fold is never worse than the per-path walk it replaces
   (it visits a subset of the same edges), and its memory does not depend on
   stage widths.

## Consequences

- Q8/Q9 of the graph-benchmark: 7.0 s -> 0.29 s and 5.5 s -> 0.29 s, answers
  identical. The remaining cost is the per-edge admission and memo probe.
- The chain-count sweep's expectation that a directed two-hop count takes no
  kernel changes: it now takes this fold.
- The fold shares its entry gates with the chain-count fold (one
  `CountEntry`), so a change to the MATCH-shape gate is made once.
- Longer chains, opposite or undirected hops keep their rungs; a weighted
  per-node dynamic programme over longer chains (relationship lineage across
  three or more hops) is out of scope.

## Open questions

- The fold takes exactly one entering row, like the chain-count fold: a
  `MATCH` fed by a driving table (rows from a preceding clause, whose start
  variable is bound per row) declines to the path fold or the streaming chain.
  Serving batches of entering rows is the natural extension — the middle-node
  memo is independent of the start, so it would be shared across rows — and is
  left to a follow-up.
- A typed-degree primitive (sealed `Entry.hops`) would make the last hop
  `O(1)` per middle node when its target admission is provable from the
  catalog; not part of this decision.
