# ADR-0537: Experimental snapshot codecs behind default-off features

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

Two measurement-only snapshot codecs compile behind default-off Cargo
features (`rkyv-codec`, `fjall-codec`) in `relate-storage` and the
facade:

- **rkyv**: one columnar struct-of-arrays archive (id columns, a shared
  name dictionary, per-row property lists over a key dictionary) with
  one lz4 block, loaded by zero-copy access.
- **fjall**: an embedded LSM keyspace (two row partitions plus a meta
  partition) holding the same cypher-json rows the JSON codec writes.

Neither is in the default build; neither is a product codec. A store
written by one is readable only by a build with its feature. A
measurement harness (`testing/relate-codec-lab`) compares every
compiled codec on one graph with a cross-codec content-equality gate.
Promoting either codec to a default or product codec is its own ADR and
requires the wide-column encodings and real-shape validation first.

## Alternatives considered

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

The engine gains two small dependency trees behind flags (rkyv + lz4,
fjall; both pure Rust, native-only, +0.1 MB and +1.0 MB respectively
when enabled). The codec vocabulary, dispatch and manifest machinery
are shared, so a fifth codec lands in the ops table alone. The
manifest version facts are codec-neutral. Measurements (fraud, pokec, LDBC SF0.1): rkyv
wins checkpoints by 2.1-4.4x and is 0.68x Parquet's size on
property-light shapes, but only 0.93-1.10x where property values
dominate — validating the archive costs the inline-value compactness of
the first layout, a trade the codec takes deliberately (a corrupt store
file is a typed error, never undefined behaviour). Load is at parity:
fastest on two of the three shapes, slower on the largest where the
validation traversal shows. fjall's size is row-overhead-bound (5.8-6.7x
on property-light shapes).
The load path (direct mapping, concurrency) remains the follow-up
investigation, not the codec. Deferred polish is tracked on the review backlog.
