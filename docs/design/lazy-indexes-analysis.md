# lazy-indexes analysis — where load memory goes, and what lazy indexes buy

Distilled from [`lazy-indexes-findings.md`](lazy-indexes-findings.md) (code investigation + measurements).
Branch `research/lazy-indexes`, base `origin/main` d50ad39. 2026-11-07.

## 1. The measurements

LDBC SF0.1, 97 MB of CSV, 327,588 nodes + 1,477,965 rels, M4 Max, `profiling` build:

| Path | Peak RSS | Steady RSS |
| --- | --- | --- |
| CSV → `:memory:` (loader) | 1.01 GB | 1.01 GB |
| CSV → store + checkpoint (119 MB on disk) | 1.15 GB | — |
| **Restore from that store** | **1.87 GB** | **1.75 GB, flat for 25 s** |

Three things follow.

**(a) Restore-from-checkpoint is the memory hog, not the write path.** Reopening a 119 MB store
leaves 1.75 GB resident — **+0.74 GB (+73%) over building the identical graph from CSV**, and it does
not decay: 25 s of query looping leaves RSS unchanged. Peak is **15.7× the store size**, ~1 KB/entity.
The documented 12 GB SF1 peak extrapolates from exactly this (11.4× the entities).

**(b) The load path's cost is allocation, interning and accounting — not query execution.**
Leaf self-time over a 24 s load window: allocator ~10%, `Interner::get` + SipHash ~14%, `memmove`/`memcmp`
~10%, per-row logical-memory accounting ~6%, property-map materialization ~5%, UTF-8 validation ~2.5%,
WAL JSON escaping; adjacency insert (`TypedAdj::insert` → `insert_hop`, verified call path) is real
work but unranked by this data. The query front end — cheap per the existing docs — does not appear.
These are **leaf** counts; caller chains above allocator frames are unwinding garbage, so no
"hottest function" claim is made here (see the symbolication caveat in the findings doc).

**(c) The deferral machinery already exists and is proven.** Value postings (equality/text/range/point)
are *already* lazy: built on first seek, then only dirty-marked per write (ADR-0063/0185). Checkpoint
restore is *already* deferred: `load_checkpoint_node` touches no index, and `finish_restore` does one
final build pass. So "lazy indexes" is not a new architecture for this codebase — it is extending a
pattern that is already load-bearing in two places to the two places that still do per-row eager work:
**the bulk write path and WAL replay**.

## 2. Where the 1.75 GB goes

The restore path holds, at once:
1. the whole shard as a `Vec<u8>` (256 MiB shard target, 1 GiB cap, 2 GiB decoded envelope),
2. Arrow `RecordBatch` buffers (row groups of 64),
3. per row: a `serde_json::Value` tree **and** a `BTreeMap<String, Value>`, before `CompactProps` —
   three representations of the same properties, two of them throwaway,
4. the growing graph,
5. at `finish_restore`: `rebuild_adj` materializes **all** live rels into one `Vec` and sorts it;
   `rebuild_indexes` calls `node_at` **per node**, materializing a full property `BTreeMap` for each,
6. then WAL replay re-applies through the *eager* per-op seams, and
7. `graph.rebuild_indexes()` runs **a second time, unconditionally** (`relate-storage/src/open.rs:347`),
   even when the WAL had nothing past the checkpoint.

Items 3, 5 and 7 are avoidable today with no format change. Item 6 is the one that argues for a
deferred apply mode. The fact that the freed transients are not returned to the OS (macOS malloc-zone
retention — the same class of bug Traverse fixed in 0.6.0) means the fix has to be "allocate less",
not "free sooner".

## 3. Where the write path's per-row cost goes

Per `CREATE` row, `install_node` does: `catalog.lock()` + `record_labels` (cloning label `String`s),
interning, logical accounting, a `count_stats` mutex bump, constraint-key seams, a chain push with a
pow2 `fit_reserved` re-fit, `index_add` (label `HashSet` insert + a `(labels+1) × props × 4` DashMap
dirty walk), `mark_node_dirty`, and then **`string_dirty_mark_node` repeats that same dirty walk a
second time**. `install_rel` adds two adjacency inserts, a degree-histogram bump and rel stats.

Bulk batching already exists (ADR-0209, threshold 10k / budget 25k) but **`collect_targets` bails to
the single-commit path whenever any constraint exists** (`exec/bulk/run.rs:134`) — so the
constraint-bearing lanes materialize an entire 50k/100k-row statement's scratch, redo and JSON WAL
payload in one commit.

Only two structures a pure-CREATE load genuinely needs per row: the adjacency hop and the chain push.
Label index, count stats, degree histograms, constraint keys and dirty sets are all foldable into a
single end-of-load pass — which is exactly what `finish_restore` already does for the restore path.

## 4. What lazy indexes would and would not buy

- **Would**: remove the per-row label-index insert + the per-row stats/constraint mutex work from bulk
  load and replay; remove one of the two wholesale rebuild passes; and make the label index (today the
  only eager index, and the only one with no lazy path) behave like the others.
- **Would not**: it is not the 0.74 GB. That is the decode-transient + rebuild-pass problem (§2).
  Index *maintenance* is per-row CPU; the RSS gap is allocation shape.
- So the ordering that the measurements support is: **fix the restore path's allocation shape first
  (biggest measured memory win), then the deferred-apply mode (CPU + the double rebuild), then the
  lazy label index (contained, format-neutral), then parallelism.**

## 5. Store format: hostile to the memory format? Partly, yes

The Parquet checkpoint stores properties as a **tagged-JSON string column** decoded per row into
`serde_json::Value` → `BTreeMap` → `CompactProps`. The live format is interned key ids + typed values.
So every restore pays a JSON parse, a `BTreeMap` build, and a **re-interning of keys that were already
interned when the graph was written** — the durable format does not carry the memory format's
interned key ids or its compact triple shape. That is the concrete answer to "is the disk format
hostile to the memory format": at the property layer, yes.

A columnar, typed, key-id-carrying shard (or the compact binary variant) would remove the double
representation and the re-interning. That is a checkpoint-format change and needs its own ADR — but
it is the change with the best ratio of memory saved to risk, and it is *independent* of the
CSR/ARENA/single-version work (which these measurements do **not** show to be the dominant term: at
~1 KB/entity resident, a ~130–150 B/entity version-chain saving is ~15% of the steady state, whereas
the restore transients are 0.74 GB of 1.75 GB).

## 6. Industry cross-check

- **Traverse (Truespar)** — the direct comparable: offline BulkWriter that emits a ready-to-load
  database file, auto-indexing at the **first MATCH after the CREATE phase** (a phase boundary, not
  per write), a memory ceiling during load plus a pre-flight check, and an explicit fix for
  allocator-RSS ratchet. Its import is a *different path* from its transactional writes.
- **Neo4j** — index population is a background phase (POPULATING → ONLINE); `neo4j-admin` incremental
  import has a dedicated build stage (prepare → build → merge); documented practice is constraints
  before load, other indexes after.
- **Memgraph** — three-phase concurrent index creation (Register → Populate → Publish), parallel
  schema recovery, parallel snapshot loading, and an **IN_MEMORY_ANALYTICAL mode that drops the MVCC
  Delta objects entirely for import** — the industry analogue of "no version overhead when there is
  only one version".
- **Kùzù / DuckDB** — indexes built explicitly after bulk copy, not maintained through it.

Common shape: **bulk load is a separate path from transactional writes, index construction is its own
phase, and version/MVCC overhead is dropped on the bulk path when possible.** Relate has the phase
pattern on restore only; the bulk write path and replay still run the transactional path.

## 7. Recommendation summary

| # | Change | Evidence it pays | Format change? |
| --- | --- | --- | --- |
| P1 | Fix restore allocation shape (stream shards, single-pass property decode, no `node_at` materialization, streaming `rebuild_adj`, drop the double rebuild) | 1.75 GB → target ≈ 1.0 GB; the +0.74 GB is measured and retained | No |
| P2 | Deferred apply mode for bulk `UNWIND ... CREATE` and WAL replay | removes per-row label/stats/constraint/dirty work and one rebuild pass | No |
| P3 | Parallel transform + parallel final build pass (apply stays single-writer) | SF0.1 load is 27 s single-threaded on 16 cores | No |
| P4 | Lazy label index (build-on-first-probe + dirty maintenance) | removes the last eager index and one full pass | No |
| P5 | Cheap per-row wins (double dirty walk, `fit_reserved` per push, interner hashing, loader endpoint resolution, WAL JSON payload) | ~25–30% of load self-time is in these buckets | WAL encoding: yes |
| P6 | Columnar/typed checkpoint shard carrying interned key ids | removes the double representation + re-interning on every open | **Yes — ADR** |

See [`lazy-indexes-proposal.md`](lazy-indexes-proposal.md) for the staged plan.
