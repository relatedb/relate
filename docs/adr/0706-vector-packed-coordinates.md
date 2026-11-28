---
type: adr
area: engine
claim: 22dd03414c3490d9d728ea690408699bc24e064a
name: 0706-vector-packed-coordinates
status: proposed
tags:
  - perf
  - search
  - values
title: Vector values carry their coordinates packed — one canonical constructor, the scan reads rows in place
REFERENCES:
  - "[[adr/0033-label-only-node-views]]"
  - "[[adr/0256-vector-search-flat-scan-rewrite]]"
  - "[[adr/0424-vector-binary-companion]]"
  - "[[adr/0705-vector-search-packed-matrix]]"
---

# 0706: Vector values carry their coordinates packed — one canonical constructor, the scan reads rows in place

## Context

`CypherValue::Vector` stores its coordinates as `values: Vec<CypherValue>`
— one full tagged enum per coordinate. Measured on the current model,
`sizeof(CypherValue)` is **48 bytes**: a FLOAT32 vector pays 48 bytes per
coordinate in memory where its numeric content needs 4, a 12× inflation.
At the embedded envelope's ceiling (ADR-0256: 1M × 768) that is ~35 GB of
coordinate storage for ~2.9 GB of numbers. The durable forms are no
better off in memory: WAL and checkpoint JSON carry the coordinates
elementwise, and the decoder expands each number into a tagged `Value` —
the packed form never exists in the process, even at load.

[ADR-0705](./0705-vector-search-packed-matrix.md) fixed the *scan* cost
with a packed f32 matrix cached per `(label, property)` — measured 5× on
the #913 workload, +3.5% process RSS for the copy — and its Alternatives
section recorded value-model packing as "not rejected, but a separate
decision: it changes every surface that matches on vector coordinates…
and it addresses memory and open time as well as scan time." This is that
decision.

[ADR-0424](./0424-vector-binary-companion.md) (proposed) established the
pattern this decision adopts: derived data lives inside the value it
derives from — born with it, updated atomically with it, nothing
scan-side to invalidate. Its insight applies to the coordinates
themselves, not only to a binary twin: a packed coordinate buffer carried
by the value needs no cache, no dirty map, and no rebuild, because the
MVCC version chain already gives every reader the exact buffer its
snapshot should see. A vector rewrite swaps the buffer; an older-snapshot
reader keeps its own.

## Decision

1. **`CypherValue::Vector` carries its coordinates packed.** `values:
   Vec<CypherValue>` is replaced by a `VectorCoords` enum with one
   `Arc`-ed slice per coordinate family, exactly as `VectorCoordKind`
   already partitions them: `Float16`/`BFloat16`/`Float32` →
   `Arc<[f32]>`, `Float64` → `Arc<[f64]>`, `Int8`/`Int16`/`Int32`/
   `Int64` → `Arc<[i64]>` (integers keep their value losslessly; the
   `vector()` constructor's homogeneity guarantee is unchanged). `coord`
   and `dim` stay. Every kind stays exact — no lossy widening, so the
   scan's decline paths (FLOAT64 carriers, non-`f32`-exact values) keep
   their meaning.

2. **One canonical constructor.** `CypherValue::vector(coord, coords)` is
   the only way a `Vector` is built — the `vector()` builtin, parameter
   decode, wire decode, serde decode, convert paths and examples all
   route through it, the rule ADR-0424 set for its companion. No site
   constructs the variant by hand, so the packed form cannot desync from
   `coord`/`dim`.

3. **The scan reads the row in place.** A vector `SEARCH` (and
   `db.index.vector.queryNodes`) holds the carrier's `Arc` and walks the
   packed slice — zero copy. There is no scan-side coordinate structure
   at all: the version chain is the invalidation. Property access on the
   scan path is a single-key compact read (`CompactProps::get(key)`),
   never the full `to_map` rebuild the graph module itself documents as
   the profile-hot path ([ADR-0033](./0033-label-only-node-views.md)'s
   avoidance pattern, applied to the vector scan).

4. **Scores stay bit-identical.** The kernels widen packed coordinates
   through the same `f64` read order, eight-lane accumulation and
   `f32`-rounding chain as the ADR-0256/0705 contract; a differential
   test pins the packed kernels to the current kernels' output bit for
   bit.

5. **Durable forms unchanged this change.** WAL and checkpoint encodings
   keep today's elementwise shape; decoders rebuild through the canonical
   constructor. The project is pre-publish, so no migration is owed, and
   a packed durable encoding (an open-time and disk-space win) stays a
   separate follow-up decision.

6. **ADR-0705's matrix fate is decided by measurement, not here.** A
   structure-free scan — packed rows read in place, single-key property
   reads, the per-query population enumeration (label sweep + sort +
   dedup) as the one remaining per-query cost — is A/B'd against the
   matrix on the #913 workload before the PR lands. Within the ADR-0245
   bands, ADR-0705's f32 path is deleted in a follow-up; if the
   enumeration residual dominates at scale, the matrix stays and becomes
   cheaper: its rows hold shared `Arc<[f32]>` clones of the values
   themselves — no second copy of any coordinate, only the id set to
   maintain.

## Alternatives considered

- **Keep `Vec<CypherValue>` and the matrix as the end state
  (ADR-0705 alone).** Rejected: the 12× in-memory inflation remains for
  every store whether searched or not, open/restore pays the expansion,
  and the matrix is a second copy plus dirty-row machinery that this
  representation makes unnecessary for the coordinates.
- **A single `Arc<[f32]>` for every kind (lossy).** Rejected: FLOAT64
  carriers and integer vectors lose exactness; the per-family enum keeps
  every `VectorCoordKind` lossless and the decline paths meaningful.
- **Pack the durable form in the same change.** Deferred: it doubles the
  encoding review (WAL, checkpoint, Parquet) for an open-time win that
  lands cleanly on top once the in-memory form is packed.
- **Intern coordinates (per-coordinate sharing).** Rejected: 48-byte enum
  headers per shared coordinate inflate worse than the list it replaces.
- **Memoize property maps generally (a node-id-keyed property-map
  cache).** Rejected for this change: single-key compact reads remove the
  scan's need without introducing a write-invalidated structure; a
  general property-map memo is a broader executor decision with its own
  invalidation story.

## Consequences

- Memory: a FLOAT32 vector's coordinate payload drops from 48 to 4 bytes
  per coordinate (12×); FLOAT64 from 48 to 8; integers from 48 to 8. The
  packed buffer is the value itself — inside the logical charge, unlike
  ADR-0705's matrix, which sits outside it.
- Construction is strictly cheaper: one `O(d)` pack per construction
  instead of one tagged `Value` per coordinate.
- Every surface that matches on vector coordinates routes through
  accessors (kind, dim, a numeric iteration) instead of the `values`
  field — 73 files touch `Value::Vector` today; most pattern matches
  collapse to accessor calls, and the Python, FFI, WASM and Arrow
  conversions convert from the packed slice, cheaper than the per-`Value`
  walks they replace.
- Equality and hashing compare coordinates numerically through the
  accessors — same results as today's per-`Value` comparison.
- ADR-0424's companion, when revived, derives its sign bits from the
  packed coordinates inside the same canonical constructor — its
  value-embedded premise unchanged, now on a representation worth
  pruning for.
- A memory-shape test ships with the change: search peak memory
  independent of carrier count, and a stored-footprint assertion for the
  packed form.

## Verification

- Unit: packing round-trips every `VectorCoordKind`; wrong-kind and
  wrong-length inputs reject as today; the packed kernels match the
  current kernels bit for bit across lengths 1–1536, four scales, both
  metrics and the all-zero row.
- Integration: differential answers (ids, order, score bits) between the
  packed scan and an oracle across the ADR-0705 differential walk's write
  shapes; the TCK and CIP-eval vector suites pass unchanged.
- Measurement: the #913 A/B harness (10k × 384, directory store) — search
  latency against ADR-0705's matrix numbers, and process RSS for the
  stored-form shrink; the structure-free scan vs matrix comparison
  recorded on the ticket before the PR.
