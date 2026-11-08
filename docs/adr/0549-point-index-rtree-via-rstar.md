# ADR-0549: POINT index backend — R\*-tree via rstar

Status: Proposed

Claim: a3f16e57a35e108edb9fea53142b1c787c35ca64

## Context

The POINT index was a fixed 1.0-unit cell hash: a `NodeId → (srid, x, y)`
map plus a `cell → HashSet<NodeId>` map, with distance seeks padded a full
cell per side. On a one-million-point WGS-84 dataset concentrated in one
metro area (90% of points inside a ~0.3° × 0.25° box), a 250 m radius probe
posted ~970k candidates for 288 matches (~0.03% precision), and an
index-backed 500 m radius query cost ~33 ms end-to-end — flat regardless of
selectivity, i.e. scan-like. The user-facing docs described R-tree behavior
the structure did not have. All declared indexes are in-memory derived
state: checkpoints persist only the index DDL, and open rebuilds postings
from the data.

## Decision

Replace the cell grid with an R\*-tree (the `rstar` crate) behind the
unchanged `PointIndex` / `DrainState` contract:

- The reverse map `NodeId → (srid, x, y)` stays: removal needs the
  coordinates to unpost, and the residual predicate owns z/SRID/wrap
  semantics.
- Full materialize (index create, reopen rebuild) bulk-loads the tree at a
  `seal()` step — the lazy string postings' seal precedent (ADR-0063) —
  instead of growing it insert by insert. The dirty-drain's incremental
  maintenance inserts and removes single leaves.
- The distance seek's envelope is the spherical disk's exact great-circle
  bounding box (the residual metric's own surface geometry), padded by a
  rounding epsilon and split across the antimeridian when the box wraps;
  the Cartesian seek keeps the flat metre box — its metric's exact bound.
  The old full-cell pad is gone. Heights qualify the bound: the residual's
  radial multiplier is `R + avg_height`, so a negative height anywhere
  (posted point or origin) can shrink the great-circle term below the
  surface bound — such an index falls back to the full posting set on
  distance seeks, and the residual answers alone.
- The residual `point.distance` / `point.withinBBox` re-check stays the
  correctness backstop; postings remain a superset the seek re-checks on
  the snapshot (ADR-0018).
- `rstar` 0.12 joins `relate-executor`: pure Rust, no build script, three
  new transitive packages (`heapless`, `hash32`, `stable_deref_trait`;
  `num-traits`, `libm`, `byteorder` were already resolved), MSRV 1.63 under
  the 1.88 pin, wasm32-clean.

## Alternatives considered

- **Adaptive multiresolution grid** (geohash-style, cell level chosen per
  query radius) — rejected: the skew flaw persists (level choice needs data
  statistics and is wrong for unit-unknown Cartesian SRIDs), there is no
  nearest-neighbour path, and bespoke heuristics replace a maintained
  library.
- **Immutable packed R-tree / KD-tree** (`geo-index`) — rejected: fastest
  queries but immutable; the dirty-drain model needs dynamic remove/insert,
  so this adds delta-buffer-plus-rebuild machinery for no query win that
  matters here.
- **Hand-rolled dynamic R-tree** — rejected: the bulk-packed static half is
  easy, but the drain needs the dynamic half (node splits, underflow
  rebalancing) whose correctness edge cases are exactly what a maintained,
  property-tested library carries; the language contract suites cannot
  guard the engine's own index internals.
- **On-disk spatial index** — rejected: every declared index is in-memory
  derived state rebuilt at open; an on-disk structure changes the
  durability model for no benefit at measured memory parity.

## Consequences

- City-scale probes go from ~970k candidates to result-proportional ones
  (measured: 393 candidates for a 288-match 250 m probe; a 500 m radius
  query end-to-end 33 ms → 91 µs). Candidate precision is ~75% at every
  radius instead of degrading with the metro's share of a 1° cell.
- Build at open: bulk load measured at grid parity (~111 ms per 1M points).
  Incremental insert is ~6× the grid's O(1) per item, bounded by drain
  batch sizes.
- Memory: measured parity (~130 MiB vs ~134 MiB per 1M points); the
  memory-shape sizing charge stands.
- The doc language ("R-tree point indexes") becomes accurate.
- The tree enables a future k-nearest-neighbour syntax (not shipped here).
- 3D/z stays in the residual (the envelope is 2D), unchanged.
