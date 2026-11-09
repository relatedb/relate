# Proposal — load-process memory, lazy indexes, parallel loading

For LABS-1110 ("swappable query and write engines"). Michael's steer: the near-term work is
**lazy indexes + concurrent loading + a more efficient conversion into the memory format**, not the
engine-swap refactor. This is the proposal from the investigation in
[`lazy-indexes-findings.md`](lazy-indexes-findings.md) / [`lazy-indexes-analysis.md`](lazy-indexes-analysis.md).

No code has been changed. Branch `research/lazy-indexes` holds the research and this proposal only.

## The three facts the proposal is built on

1. **Restore-from-checkpoint is the memory hog.** SF0.1: reopen a 119 MB store → **1.87 GB peak,
   1.75 GB steady** (flat for 25 s) versus **1.01 GB** to build the identical graph from CSV.
   **+0.74 GB, retained, not a spike.** 15.7× the store size, ~1 KB/entity.
2. **Index construction is already lazy in the right places.** Value postings are build-on-first-seek
   (ADR-0063/0185); restore already defers everything to one `finish_restore` pass. The gap is the
   *bulk write path and WAL replay*, which still run the transactional eager path per row.
3. **The durable property encoding is hostile to the memory format.** Parquet carries properties as a
   tagged-JSON string column; every restore pays `serde_json` → `BTreeMap` → `CompactProps` plus a
   **re-interning of keys that were interned when the graph was written**.

## Stages

Ordered by measured value. Each stage is independently landable and independently measurable.

### S1 — Fix the restore path's allocation shape (no format change) ← start here

- Stream the shard instead of reading it whole into a `Vec<u8>` (`snapshot/native/mod.rs`,
  `read_bounded_object`); decode row group by row group.
- One-pass property decode: `serde_json` → `CompactProps` directly, dropping the intermediate
  `BTreeMap<String, Value>` (`codec/parquet.rs`).
- `rebuild_indexes`: stop materializing a full public `Node` per node via `node_at`
  (`relate-executor/src/graph/index.rs:223-228`) — walk live versions in place.
- `rebuild_adj`: replace the all-rels `Vec` + sort with a streaming/chunked build.
- Delete the unconditional second `graph.rebuild_indexes()` after WAL replay
  (`relate-storage/src/open.rs:347`); run it only when replay applied records.
- **Acceptance**: SF0.1 restore steady RSS ≤ 1.15 GB (from 1.75 GB) and peak ≤ 1.3 GB, measured with
  the `ps -o rss=` polling method in the findings doc, on the `profiling` build; the same store's
  logical bytes unchanged; the existing open/restore tests green.
- **Why first**: it is the largest measured win, it needs no format change, and it is the prerequisite
  for making the 12 GB SF1 figure tractable.

### S2 — A deferred (bulk) apply mode for the write path

- The restore path already proves the pattern (install raw versions, then one build pass). Add the
  same mode to bulk `UNWIND ... CREATE` and to `apply_replayed_redo`: skip per-row label-index inserts,
  count/degree/constraint-key updates and dirty marking; accumulate touched id ranges; fold once at the
  end.
- Batch the per-row Mutex acquisitions (`catalog.lock()` + `record_labels`, `count_stats`,
  `dirty_nodes`) to one per batch instead of one per row.
- Fix `collect_targets` bailing to the unbatched path whenever a constraint exists
  (`exec/bulk/run.rs:134`) — that is what makes constrained 100k-row statements materialize whole.
- **Acceptance**: SF0.1 load steady RSS and wall-clock both improve; `bulk-loadtest` create/merge lanes
  keep their rows/s within 5% and drop peak RSS; the 100k constrained lanes stop materializing a whole
  statement.
- **Note**: this is the same shape as LABS-1110's `GraphImport` trait — doing S2 first de-risks that
  phase and gives the seam a real consumer.

### S3 — Parallelism: transform and build in parallel, commit stays single-writer

- Parallelize (a) shard/CSV decode + property transform into batch-local form, (b) the final
  index/adjacency/stat build pass sharded by node-id range (Memgraph's parallel label-index precedent:
  cap threads by batch count), (c) the loader's endpoint resolution.
- Do **not** parallelize `apply_redo` — the single-writer choreography (publication write lock, WAL
  mutex, group leader) is load-bearing; the containers (DashMaps) would tolerate it, the choreography
  will not.
- Loader win available today: `UNWIND $rows AS r MATCH (a) WHERE elementId(a) = r.s MATCH (b) WHERE
  elementId(b) = r.e CREATE ...` costs two endpoint resolutions per relationship (~3M lookups at SF0.1);
  carry the node ids through the batch instead.
- **Acceptance**: SF0.1 load wall-clock (27 s single-threaded on 16 cores) reduced with byte-identical
  results; no change to commit ordering guarantees (existing follow/commit suites green).

### S4 — Lazy label index (the literal "lazy indexes" ask, made precise)

- The label index is the only eager index left: a `HashSet` insert per node per label on every write,
  plus a full rebuild pass. Give it the same treatment as value postings — build on first probe, then
  dirty-maintain — or fold it into the deferred build of S2 for bulk paths.
- **Acceptance**: label-probe queries unchanged in results; per-row label-index work gone from the
  profile of a pure-CREATE load; the rebuild pass count drops.

### S5 — Cheap per-row wins (can land alongside S1–S4)

- Remove the duplicated `declared_dirty_pairs` walk — `index_add` and `string_dirty_mark_node` run the
  same walk twice per row.
- `fit_reserved` pow2 re-fit on every push → reserve by batch size.
- Interner hashing: `Interner::get` is the largest engine leaf in the load profile and its cost is
  SipHash; use the already-present `FxHasher` (or a batch-intern API on the load path).
- `Value`/`Arc<Vec<String>>`/`Arc<PropertyMap>` redo clones per row.
- WAL payload: JSON-encoded per commit and held for the whole group. For bulk load this is pure
  overhead — a compact/binary record or a bulk-segment record removes it. **WAL format change → ADR.**

### S6 — Checkpoint property encoding (the disk-format answer, own ADR)

- Carry interned key ids and the compact triple shape in the shard instead of a tagged-JSON string
  column, so restore does not parse JSON, build a `BTreeMap`, and re-intern keys.
- This is the "the disk format is hostile to the memory format" finding. It is a checkpoint-format
  change and needs an ADR before implementation; it is independent of the CSR/ARENA/single-version work.

## Non-goals (explicitly out of scope here)

- **Memory store format**: version chains, CSR, ARENA. Impact assessed, not changed (below).
- **The engine-swap refactor** (LABS-1110 PR #5) — untouched; S2 is designed to feed its `GraphImport`.

## Store-format impact assessment (investigated, not proposed)

- **Single-version chains** ("remove versions if there is only one"): ~48 B charged chain overhead +
  one heap `Vec` + 16 B stamps per entity, with a change blast radius of ~8 sites. At ~1 KB/entity
  resident this is **~15% of steady state** — real, but an order below the 0.74 GB restore transient.
  Worth doing, not first.
- **CSR**: ADR-0206 already rejects CSR as the primary adjacency store (ts-DESC pushdown, visibility,
  single-writer redo path), and these measurements do not contradict that. What the code map *does*
  support is a **bulk append path** for `insert_hop` (today `binary_search` + `Vec::insert` shift per
  hop, verified as a real load-path call chain — `store.rs:702-705` — though the profile does not rank
  it): append unsorted during bulk load, sort once at the end.
- **Tiered arena**: ADR-0206's first precondition; unaffected by this proposal, and the measurements
  give it a clearer bar (the arena's per-node `Vec` headers are part of the ~1 KB/entity).

## What Traverse does (the direct comparable)

In-memory Rust graph DB, Neo4j-compatible, `.tvdb` single file, snapshot default + optional WAL. Its
answer to exactly this problem: an **offline BulkWriter** (`traverse-admin`) that emits a ready-to-load
database file, and **auto-indexing at the phase boundary** — without `--index` flags the importer waits
for the first `MATCH` after the `CREATE` phase, then indexes every (label, property) pair it found.
Plus a memory ceiling during load, a pre-flight memory check, and an explicit fix for allocator RSS
ratchet. Its bulk import is a *different path* from its transactional writes — which is the S2 shape.

## Open questions for you

1. **Order**: start with S1 (restore) or S4 (lazy label index, the literal ask)? I recommend S1 —
   it is the measured 0.74 GB and it makes SF1 openable in a smaller box.
2. **S6 (checkpoint encoding)**: do you want that opened as an ADR now, or kept as a follow-up until
   S1/S2 land? It is the biggest structural lever but the largest change.
3. **Bulk-load entry point**: is a dedicated bulk-load surface (à la Traverse's BulkWriter / an offline
   builder) in scope, or does everything stay on the existing Cypher `UNWIND` path?
4. **SF1 run**: do you want me to run SF1 (≈10 min + 241 MB download) to confirm the extrapolation, or
   is SF0.1 enough for the decision?
