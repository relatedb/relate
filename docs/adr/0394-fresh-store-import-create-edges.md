---
type: adr
area: cli
claim: eb67ed1b12d2de3f5cb5550014003686b963f152
name: 0394-fresh-store-import-create-edges
status: proposed
tags:
  - load
title: Fresh-store imports CREATE their edges
REFERENCES:
  - "[[adr/0376-git-to-graph-import]]"
  - "[[adr/0392-git-import-per-batch-commit]]"
---

# ADR-0394: Fresh-store imports CREATE their edges

## Context

[ADR-0392](./0392-git-import-per-batch-commit.md) moved `lab git-to-graph` to per-batch transactions over lazily
generated rows, and its Later-references note on [ADR-0376](./0376-git-to-graph-import.md) records that the
import's `MERGE`/`SET` semantics stand. A full import of a real
large-checkout history (`~/Development/neo4j`: 91,532 commits, 169,381
files, 1,171,478 `TOUCHED`, 2,498,086 `CO_CHANGED`) measured 604 s for the
write phases on that design, 96% of it in one place: the executor's
existence check for an edge `MERGE` between two already-bound nodes runs
the generic pattern walker — it enumerates an endpoint's typed neighbors
and, per visited neighbor, binds a row, compares property strings and
clones property vectors. Per row that is O(the endpoint's degree), and a
real history is heavy-tailed: the hottest file is touched by 1,107
commits, the most prolific author has 8,386. `TOUCHED` rows cost ~372 µs
apiece against `CREATE`'s measured ~8 µs on the same data.

The importer knows something the executor cannot: on a store with no
nodes, its edge rows are distinct by construction — one `AUTHORED` per
commit, `TOUCHED` pairs deduped per commit, `CO_CHANGED` rows the distinct
keys of the pair-count map. A local experiment wrote the same 10,000-commit
window both ways and produced identical graphs: `MERGE` 19.7 s for the
three edge phases, `CREATE` 4.8 s, with every node and edge count equal.

One input had to change for that knowledge to be sound: `git log
--name-only` can list one path twice for a commit, and the plan carried
the raw list, so duplicate `TOUCHED` rows were possible — harmless under
`MERGE`, illegal under `CREATE` (they would land parallel edges). The plan
now dedupes each commit's paths in first-seen git order.

## Decision

On a store that holds no nodes, the three edge phases of a `lab
git-to-graph` import run `CREATE` statements instead of `MERGE`: the rows
are distinct by construction, so the edge `MERGE`'s existence walk is work
the importer can prove unnecessary. The node phases keep `MERGE` (they are
not the cost), and any populated store — a re-import, a re-run over a
partially imported history, a store with unrelated data — keeps the
`MERGE` path, so [ADR-0392](./0392-git-import-per-batch-commit.md)'s re-run-completion semantics are unchanged. A
freshness check (one `count(n)` read) runs before the range-index DDL and
the first batch, and the final graph is statement-path-independent: a test
pins the `CREATE` import and the `MERGE` re-import to identical counts.

Per-commit path dedupe applies on both paths, so `files_changed` becomes
the distinct file count of the commit.

## Alternatives considered

- **Fix the executor's bound-endpoint edge `MERGE` (compare `NodeId`s
  over the adjacency instead of walking with row binding).** The
  principled fix: it also repairs re-imports and any user query that
  `MERGE`s an edge between bound nodes. Rejected for this change: it is
  executor surgery with its own TCK safety proof, while the importer can
  legally skip the walk today. It remains the follow-up this ADR points
  at.
- **Larger write batches.** Rejected: measured per-batch fixed overhead is
  ~2–3 ms; the cost is per row, not per batch.
- **Defer the exit checkpoint to save the import's tail.** Rejected: the
  checkpoint measured 1.8 s at this scale and the next open must stay
  fast; there is nothing to defer.
- **Keep `MERGE` everywhere and accept the minutes.** Rejected: a demo
  import of a real checkout should land in about a minute; 96% of the
  time is a walk whose result the importer already knows on a fresh
  store.

## Consequences

- A first import into a fresh store writes `CREATE` edge statements; the
  graph it leaves is identical to the `MERGE` path's, pinned by tests that
  run both and compare counts, and by the re-import test (`CREATE` run,
  then `MERGE` re-import, same graph).
- Re-imports and re-runs over partial imports are unchanged ([ADR-0392](./0392-git-import-per-batch-commit.md)),
  and their speed is unchanged until the executor's bound-endpoint
  `MERGE` is fixed as a follow-up.
- The import's transient-overhead budget ([ADR-0392](./0392-git-import-per-batch-commit.md), 734,003,200 B) is
  unchanged and now has more headroom: `CREATE` skips the walker's
  per-candidate allocations.
- A commit that lists one path twice now contributes one `TOUCHED` row
  and a `files_changed` count of distinct files, on both statement paths.
- `docs/api/cli/spec.md` and the lab help text state the fresh-store
 `CREATE` and the `MERGE` re-import in the same change; [ADR-0392](./0392-git-import-per-batch-commit.md) gains a
  Later-references note naming this ADR.
