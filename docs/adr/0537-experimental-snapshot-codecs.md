# ADR-0537: Experimental snapshot codec behind a default-off feature

Status: Proposed
Claim: adcb1136ef022207ffd7bfd8cff8a3be3a3779fa

## Context

The durable snapshot format is Parquet (ADR-0105) with JSON as the
always-built fallback. The `arrow`+`parquet` dependency tree costs 8.2 MB
of a 25.6 MB FFI library — a third of the binary — and the format choice
was never measured against alternatives (zero-copy archival, embedded
keyspaces). Candidate codecs need real measurements (store bytes,
checkpoint and load wall, binary cost) on real graph shapes before any
promotion decision, and measuring them requires them to exist in the
engine, not in a fork.

## Decision

One measurement-only snapshot codec compiles behind the default-off
`rkyv-codec` Cargo feature in `relate-storage` and the facade:

- **rkyv**: one columnar struct-of-arrays archive (id columns, a shared
  name dictionary, per-row property lists over a key dictionary) with
  one lz4 block, loaded by zero-copy access.

It is not in the default build and not a product codec. A store written
by it is readable only by a build with the feature. A measurement
harness (`testing/relate-codec-lab`) compares every compiled codec on
one graph with a cross-codec content-equality gate. Promoting the codec
to a default or product codec is its own ADR and requires the
wide-column encodings and real-shape validation first.

A second candidate — an embedded-LSM keyspace codec (the `fjall` crate)
holding the same cypher-json rows the JSON codec writes — was measured
alongside rkyv and dropped; the measurements are the rejection record
in Alternatives considered. Its code is preserved under the
`experimental/fjall-codec` git tag.

## Alternatives considered

- **fjall, an embedded LSM keyspace codec — measured and dropped:** its
  rows are the JSON codec's cypher-json (property names repeated per
  row, one LSM entry and one lz4 block per row), so the store is
  row-overhead-bound: 5.8-6.7x Parquet's bytes on property-light shapes
  against rkyv's 0.68x; checkpoint 5.1 s and load 4.5 s against rkyv's
  0.56 s and 1.73 s on the same shape; +1.0 MB binary against rkyv's
  +0.1 MB; the only codec that refused read-only opens and required a
  native directory. Its one structural advantage, in-place incremental
  writes, is unused by the replacement-semantics checkpoint (staging
  keyspace + rename swap), so the LSM costs were paid without the
  benefit. No measured dimension favored it.
- Promote rkyv as the default codec now — rejected: the v1 archive has
  no per-column encodings, the measured shapes are synthetic-heavy, and
  a default swap without an interop story (Parquet remains the exchange
  format) is not reversible once stores exist.
- Measure in a fork or out-of-tree harness — rejected: the codec
  contract (VFS, WAL interaction, catalog and ontology riding the
  checkpoint image) is engine-internal; an out-of-tree copy would
  measure a different thing and drift.
- Do nothing; keep Parquet unmeasured against alternatives — rejected:
  the binary cost is a third of the shipped library and the trade was
  never quantified.

## Consequences

The engine gains one small dependency tree behind a flag (rkyv, lz4,
indexmap; pure Rust, native-only, +0.1 MB when enabled). The codec
vocabulary, dispatch and manifest machinery are shared, so another
codec lands in the ops table alone. The manifest version facts are
codec-neutral. Measurements (fraud, pokec, LDBC SF0.1): rkyv wins
checkpoints by 2.1-4.4x and is 0.68x Parquet's size on property-light
shapes, but only 0.93-1.10x where property values dominate — validating
the archive costs the inline-value compactness of the first layout, a
trade the codec takes deliberately (a corrupt store file is a typed
error, never undefined behaviour). Load is at parity: fastest on two of
the three shapes, slower on the largest where the validation traversal
shows.
The load path (direct mapping, concurrency) remains the follow-up
investigation, not the codec.
