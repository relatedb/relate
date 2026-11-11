# ADR-0573: Bulk window-local adjacency install for checkpoint load

Status: Proposed
Claim: c2df48b18a96c91468db765422b95fd312b73020

## Context

A checkpoint restore installs the graph row by row: the import seam hands the
engine one node or one relationship at a time, and the executor inserts each
relationship's hop into the adjacency chunk of one of its endpoints. That
insertion is a binary search plus a vector insert that keeps the chunk's
timestamp order, and every row interns its own labels and property keys as it
arrives.

The cost is measured, not assumed. On a 100k-node, 1.77M-relationship store,
install is codec-independent at roughly 1.5–1.7 s of a ~2 s load, and the
profile's install-side leaves are dominated by the per-hop insert and the
per-row interning. Install is not the whole wall: on the same measurement,
decode and install are the same size, so a design that parallelizes only
install leaves half the load untouched.

Storage writes parts as contiguous id windows (ADR-0534, ADR-0553), a part's
rows being a run of one window's ids in id order, batched at the row-group cap
(ADR-0563). Id windows are therefore disjoint units of work that already exist
in the store layout, and node windows need no new invariant to become parallel
install units.

## Decision

1. **The import seam gains a bulk spelling.** The row-at-a-time calls stay, with
   a default implementation that loops them, so every existing implementation
   keeps working. The executor overrides the bulk calls with a window-local
   builder.
2. **Node windows install in parallel; the publication boundary stays the single
   writer.** Windows are id-disjoint by construction, so workers can each build
   one and hand the result over. Nothing half-installed is ever visible.
3. **Relationship adjacency is not a disjoint-unit problem.** Rel windows are
   rel-id-disjoint, and a relationship's hop lands in its endpoints' chunks, so
   a high-degree node's chunk receives hops from relationships spread across
   many windows. Installing relationship adjacency in parallel therefore needs a
   merge discipline: per-worker local per-(node, type) slices merged at install,
   or a two-pass split that installs version state per window and groups
   adjacency by endpoint afterwards.
4. **A window-local build lands in the existing chunk shape.** The walk kernels
   read the per-(node, type) chunks the row-at-a-time path fills, in the same
   timestamp order, so the bulk path must produce exactly those chunks. The win
   is the removed per-hop search and per-row interning, not the insert's
   memmove, which the bulk path still pays.
5. **Decode parallelizes on row groups, scheduled on the page envelope.** A row
   group is the unit the writer already batches by and is far too small to
   schedule individually, so decode units coalesce to the writer's page-envelope
   row count. A row group's id range is read from its id column, never computed
   by stride: the encoder skips ids invisible at the read timestamp before
   batching, so groups have gaps.

## Alternatives considered

- **Parallelize decode only, leaving install serial.** Rejected: decode and
  install are now the same size, so this caps the win at half the wall, and the
  row-at-a-time install seam is what blocks both.
- **Partition by row group instead of by id window.** Rejected: a row group is
  too small a scheduling unit, and it does not isolate relationship adjacency —
  hops from many groups land in one node's chunk — so it inherits the merge
  problem without the disjointness that makes the coarse axis cheap.
- **Compute a row group's id range by stride from its part's window.** Rejected:
  groups are runs over the visible subset, so their ids have gaps; a computed
  range is wrong on any store with deleted or invisible rows.
- **Read the store through a memory-mapped representation instead of installing
  it.** Rejected as this decision's alternative, not as an idea: the walk kernels
  read the in-memory chunk shape, so a mapped store is a second representation
  with its own equivalence obligation — a separate decision that does not remove
  the install this ADR parallelizes.

## Consequences

- Install cost falls toward the decode cost, and the two can then be pipelined or
  fused per worker.
- The publication and atomicity discipline is unchanged: a restore still
  publishes once, and a half-built graph is never observable.
- The relationship-side merge discipline is the expensive half and is not
  designed here; node parallelism can land without it, as can the serial tails
  after install (the index rebuild and the restore finish), which this decision
  does not cover.
- Chunk-shape and ordering equivalence becomes a test obligation: a parallel
  install must be indistinguishable from the row-at-a-time one.
