---
type: adr
area: engine
claim: 8d2bd89e103f79165c527a1920b3a108031c9747
name: 0375-profile-walk-start-and-store-read-kinds
status: proposed
tags:
  - observability
title: PROFILE names the walk's start and the store-read kinds
REFERENCES:
  - "[[adr/0344-query-plan-node-tree-and-per-operator-metrics]]"
  - "[[adr/0345-plan-surface-alignment-cli-bolt-http]]"
  - "[[adr/0350-bound-node-pivot-for-pattern-expansion]]"
  - "[[adr/0351-cheapest-element-start-for-unbound-patterns]]"
  - "[[adr/0355-fxhash-core-maps-and-interned-domain-reads]]"
  - "[[adr/0397-reads-cell-legibility]]"
---

# ADR-0375: PROFILE names the walk's start and the store-read kinds

## Context

The rich plan tree ([ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md)/[ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md)) reports one `Db Hits` number per
operator and one `Details` string. Two diagnoses it cannot make, both
measured on the LDBC workload:

- **q3 regressed 0.03 ms → 78 ms** when an anchored point lookup was
  displaced and the walk traversed ~135k relationships backward. PROFILE
  showed only `Expand: Rows=0, DbHits=135701, TimeNs=112ms` — the walk's
  start element, its direction and *why* it was chosen were invisible, and
 the walk can now start at any element ([ADR-0350](./0350-bound-node-pivot-for-pattern-expansion.md)'s bound pivot, [ADR-0351](./0351-cheapest-element-start-for-unbound-patterns.md)'s
  cheapest start), so a displacement is a first-class outcome.
- **The `Db Hits` total hides its mix.** 135,701 hits from adjacency hops
  versus from node-store reads are different problems; "Rows=103 but 300k
  relationship hops" is the one-glance diagnosis for the
  scan-turned-traversal family (the same shape as the LABS-328
  to_map-per-row finding).

The counters already existed (`HOP_EXPANDS`, `NEIGHBOR_WALKS`,
`NODE_READS` — LABS-375) and the start choice is already computed at one
site (`match_resolved_path`'s selection).

## Decision

1. **The walk's chosen start rides the per-clause metric record.** A
   `WalkStart { element, backward, reason }` with a closed reason
   vocabulary — `bound-pivot`, `cheapest`, `seek-anchored`, `index-0` —
   recorded once per clause by the walker (the clause's first walk wins)
   and taken at the metric tick.
2. **The read kinds ride it too**: `StoreReads { hops, neighbor_walks,
   node_reads }`, snapshotted at the clause boundaries (three thread-local
   reads per clause, never per row). The `Db Hits` total is the kinds'
   sum, derived rather than snapshotted a second time.
3. **The wire carries both as plan args** — `Hops`, `NeighborWalks`,
   `NodeReads` (with the metric set) and `StartElement`, `StartBackward`,
   `StartReason` (when a walk ran, only on a PROFILE node) — validated
   loudly at the accessor, like the metric quartet and the warning names.
4. **The rendered table gains a `Reads` column** (`135000h 0w 701n`,
   inside the metric column group) and the walking operator's `Details`
   cell gains a `[start #k, backward, bound-pivot]` suffix. The CLI, the
   machine modes, Bolt and HTTP all read the same wire args.
5. **No hot-path cost**: the counters are pre-existing thread-locals; the
   additions are three thread-local reads per clause at the tick plus one
   `Option` write per walk. The estimate-derivation path that does cost
   real time (LABS-351) is untouched.

## Alternatives considered

- **A warning per displaced start** (the [ADR-0355](./0355-fxhash-core-maps-and-interned-domain-reads.md) notification channel) —
  rejected: a displaced start is not a defect; the cheap-start heuristic
  makes it correct behaviour, and a warning would fire on healthy plans.
- **A column per read kind** (`Hops`, `Walks`, `Node reads`) — rejected:
  three more columns widen every PROFILE table for a fact one compact
  cell carries; the kinds are read together, not compared across rows.
- **Recording the start per row** (the pivot depends on the row's
  binding) — rejected: the metric is per clause, and a per-row write is a
  hot-path cost for a diagnostic; the clause's first walk is the
  representative one.
- **Deriving the start at render time from the plan shape** — rejected:
  the choice is runtime (the cheapest-start estimate reads live statistics
  and the row's bindings), so it cannot be recovered from the tree.

## Consequences

- `cypherlite::ClauseMetrics` grows `reads` and `start`; the executor's
  `StoreReads`/`WalkStart`/`StartReason` are re-exported by `cypherlite`
  so a consumer can name the field types.
- The plan wire gains six args; the CLI spec, the rust spec and the plan
  table change with it.
- The `Reads` column rides the metric column group, so `METRIC_COLUMNS`
  is the group's one width (five columns).
- The per-clause granularity still blends a scan with the expansion it
  feeds (LABS-343 splits the tree per physical operator, where this
  breakdown becomes per-operator precise).

## Later references

- [ADR-0397](./0397-reads-cell-legibility.md) supersedes this ADR's cell format for the table presentation
  only: the column header names the kinds in full words and the cell
  renders the nonzero kinds. The wire fields (`Hops`/`NeighborWalks`/
  `NodeReads`) and the `cypherlite.plan/v1` document fields are unchanged.
