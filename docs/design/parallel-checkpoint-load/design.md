# Premise addendum — verified 2026-11-18 against origin/main `2e47f55e`

This document was written 2026-11-10 against main `2f293ba`. Verified when the
feature was charted (file:line citations against current main):

- **The install path no longer touches adjacency.** `load_checkpoint_rels`
  (`relate-executor/src/graph/restore.rs`) restores a plain row: id checks,
  `LiveRel::from_parts_cached` (one interner lookup per distinct type and
  label per slice), one catalog lock per slice, and the `IdVec` entry insert.
  `insert_hop` is not on the restore path.
- **Adjacency is built after the load by `finish_restore` → `rebuild_adj`,
  already sharded by node (PR #207) and emitting the durable folded shape
  (ADR-0608 seal pass, PR #412).** The "merge-disciplined rel install"
  section of the original design — hub chunks receiving hops from many id
  windows, k-way merge, two-pass split — is **obsolete**: rel rows are
  id-disjoint window units exactly like node rows.
- **Version chains cost nothing on initial load**: single versions are
  `Chain::Inline` with zero heap (`graph/chain.rs`, compile-time size assert).
- **Interning on load is per-row but write-once**: `Interner` is a DashMap;
  `intern` writes only on first sight, so restore pays a shard hash lookup
  per string per slice, not duplication. The query-side interning epic
  (planner tokens) did not touch the restore path.

The concurrent open landed along both axes this addendum predicted: one
`ParquetRecordBatchReader` per worker-span row group (`codec/parquet/span.rs`),
one store lock per install slice (`GraphImport::load_checkpoint_nodes` /
`load_checkpoint_rels`) with a per-slice `NameCache`, and the post-install
phases on `shard::run_shards`. The original document's premise — install
dominating the load, so decode is not the lever — was superseded by the
post-`ROW_GROUP_CAP` profile: decode and install had become the same size,
so both carried a worker.

## Still actionable

Measured remainders, not yet taken:

- **The serial remainder inside `finish_restore`** is `materialize_rel_rows`
  (one row buffer; sharding it needs a flatten copy that cancels the win),
  the shared endpoint label cache (one read-once map — per-shard caches
  re-read the far endpoint, the measured regression), and the two
  single-pass modulo partitions.
- **`rebuild_indexes` is 11.8% of the pokec:medium wall** (`0.0146` on
  fraud). It was outside the serial-tails node's fraud-defined scope and
  remains untouched.
- **The WAL-replay tail after a snapshot restore is unquantified.** A
  snapshot+tail reopen pays it after the install; measure before designing
  around it.
- **The wide format's decode cost** (`byte_view_array` /
  `primitive_array` / `dictionary_array` leaves) is a codec-side lever, not
  an open-flow one.

Measurement discipline for any follow-up: one open per process, best of 3,
release build, and stamp the 1/5/15-minute load averages — this box is
shared, and load swings by an order of magnitude between windows. fsync is
not in this path (the store is read-only on reopen), so the win here is
CPU-side only.
