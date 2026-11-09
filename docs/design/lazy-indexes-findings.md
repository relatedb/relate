# lazy-indexes findings — LABS-1110 direction investigation

Branch `research/lazy-indexes` (worktree /Users/mh/d/rust/relate-wt/lazy-indexes), base origin/main d50ad39, 2026-11-07.
Scope from Michael: not the full engine-swap refactor — **lazy indexes** + parallel/concurrent
loading + efficient disk→memory conversion as the actual near-term work under LABS-1110's frame.
Store-format changes (single-version drop, CSR, ARENA) out of scope; impact assessment only.

## Ticket frame (LABS-1110, "Swappable query and write engines", PR #5)

- Goal: today's code becomes the in-memory query engine + local write engine behind
  `relate-engine-api` traits (`QueryEngine`, `WriteEngine`, `GraphExport`/`GraphImport`);
  `relate-storage` stops depending on `relate-executor`; facade holds an `Engines` enum.
- User steer: before/alongside that seam, the real win is the **load process**: lazy indexes,
  parallel loading, efficient conversion into the memory format.

## Repo facts (ADRs + docs)

- **Durable model**: fsync'd local WAL (`RedoOp`) + checkpoint snapshot; Parquet default
  codec, JSON GraphData alternative (ADR-0004 → 0012, ADR-0050). `Database::open` =
  load snapshot → replay WAL.
- **Live store** (ADR-0033, Accepted): interned labels/types/keys; compact node records +
  compact property lists (no per-version BTreeMap); typed-CSR-ish **per-(node, rel-type,
  direction) chunks**; `AdjHop { ts: Option<i64>, id: RelId }` = 16 B/hop, uncompressed,
  kept in **ts-DESC order** (ADR-0035) for newest-first expansion (ADR-0203). Lists are a
  visibility superset: deletes don't unlink; expand filters.
- **ADR-0206 (Proposed) — compressed topology**: measured 96–99% topology-twin savings,
  decode 33–173× off critical path; conditional on (1) transactional delta-store design,
  (2) **tiered-arena prototype** (one Vec<AdjHop> arena per node-direction + (type,off,len)
  index). CSR-as-primary rejected (ts-DESC pushdown, visibility, single-writer redo path).
  These are the open ARENA/CSR items the user referenced.
- **Known load cost** (docs/datasets.md): LDBC SF1 load = 241 MB archive, 3.18M nodes,
  17.26M rels, ~9.5 min release, **12 GB peak RSS** (~50× the archive, ~4 KB/entity) —
  the load process is the memory bottleneck headline.
- **Profiling recipe exists**: `cargo build --profile profiling -p relate-datasets --bins`;
  samply on macOS (no sudo); profile `--uri :memory:` (on-disk is ~85% fsync wait).
  Prior fraud in-memory profile: front end ≈ 6% of samples; cost sits in executor property
  materialization.
- **Loadtest surface**: `bulk-loadtest` (single-threaded by design; create/merge/update/delete
  ladder 5k..100k rows per statement, one commit per batch, peak RSS per measurement);
  `fraud-loadtest --workers` (concurrent group-commit shape); `memory-shape-loadtest`
  (serve-process RSS vs logical bytes per shape, ADR-0289; dense ring ~42× RSS ratio
  observed at release); `processing-memory-loadtest` (ADR-0333).
- LABS-858 (Todo): explicit txns > 64 MiB can't commit (one WAL record per txn) — bulk
  loads in one BEGIN/COMMIT fail at COMMIT. Relevant to any bulk-load mode.
- LABS-853 (Done): lazy entity materialization on the read path (range seeks, walks) —
  lazy on read side exists; **no lazy anything on the load/index side** (pending code check).

## Industry research (web, 2026)

### Traverse (Truespar) — the direct comparable
- In-memory Rust graph DB; openCypher + ISO GQL, Bolt 5/6, Neo4j drivers unchanged;
  embedded/server/WASM; one portable compressed `.tvdb` file per DB; snapshot persistence
  default + optional WAL; hot-swap reload; mimalloc with bulk-free + memory ceiling during
  load + pre-flight memory check (0.6.0 fixed mimalloc RSS ratchet, load leak).
- **BulkWriter / traverse-admin**: builds `.tvdb` offline (CSV or Cypher script, no server),
  "orders of magnitude faster" than loading through Cypher statements.
- **Lazy-ish auto-indexing**: without `--index` flags the importer watches for the **first
  MATCH after the CREATE statements**, then scans loaded data and indexes every
  (label, property) pair found — i.e. index build happens at a phase boundary, not per
  write. With `--index`, indexes are built before the first statement (helps MERGE-heavy
  scripts). traverse-bench import: "indexes are created automatically before data import".
- Pokec 100k/1.77M: 300–850 MB RSS (Neo4j 1.2–2.8 GB, spikes 17 GB); multi-core
  parallel execution of large queries; pipelined Bolt import (512 stmts/batch).

### Neo4j
- `CREATE INDEX` → background **population** (POPULATING → ONLINE, populationPercent);
  index unusable while populating; `db.awaitIndexes`. Import best practice: constraints
  (implicit range indexes) BEFORE load, extra indexes AFTER load (async populate).
- neo4j-admin incremental import has a dedicated **build stage**: prepare → build
  (indexes; DB read-only) → merge (brief write lock). I.e. index construction as a bulk
  phase is the industry shape.

### Memgraph
- **Three-phase concurrent index creation** (ADR 004): Register (brief READ_ONLY) →
  Populate (concurrent with writes; MVCC snapshot isolation; cancellable) → Publish
  (atomic, planner-visible, plan-cache invalidation). Skip-list indexes are concurrently
  writable, so population overlaps ongoing inserts.
- **Parallel schema recovery** (`storage-parallel-schema-recovery`): constraints/indexes
  recovered in parallel at startup; PR #882 also parallelized snapshot loading (labels,
  properties, connectivity) and parallel label-index creation (threads capped by batches).
- **IN_MEMORY_ANALYTICAL** mode: skips Delta (MVCC) objects entirely for fast import —
  the industry analogue of "remove versions when there's only one".
- Auto-index creation flags queue async index creation on first sight of a new label/type.

### Other
- DuckDB: ART indexes maintained on insert for ACID tables; bulk/parquet paths bypass
  indexes (indexes built per-statement when constraints demand; scans don't need them).
- Kùzù: COPY FROM parallel chunked CSV import; indexes optional and built per explicit
  CREATE after load (not maintained during copy by default).
- TigerGraph: loader parallelism across machines/threads with a separate loading job graph;
  upsert semantics; beyond Relate's embedded scope — cited as parallel-load precedent.

### Pattern synthesis
1. Nobody bulk-loads through the transactional per-row path when they can avoid it;
   the fast path is an offline/bulk builder that produces the memory image or its file.
2. Index construction is a **separate phase** after data load (Neo4j build stage/population,
   Traverse auto-index at first MATCH, Kùzù explicit CREATE, DuckDB constraint-driven).
   Concurrent-with-writes population (Memgraph) is the advanced form for online DBs.
3. MVCC/version overhead is dropped on the bulk path when possible (Memgraph analytical mode).
4. Parallelism is applied to (a) decode/transform of input, (b) per-shard/index build
   (Memgraph parallel label-index creation), not to the single-writer commit path.

## Code investigation

Two delegated passes, all paths relative to the worktree root (engine crates live under `relate/`).

### A. Load / write path

**Harness load is serial.** `testing/relate-datasets/src/loadtest.rs run()` — generate → `Database::open_with`
→ `dataset.load(&mut conn)` on the MAIN thread (one connection) → only the *measured* phase spawns
`workers`. `load.rs unwind_create` chunks into `BATCH = 200` rows and issues one autocommit
`UNWIND $rows ... CREATE` → **one commit per 200 rows**. Fraud/identity create RANGE/TEXT indexes
*after* the node load, so each `CREATE INDEX` pays a full label-bucket scan.

**Per-CREATE-row, execution** (`relate/relate-executor/src/overlay/writes.rs create_node`): label
deltas, workspace charge, `alloc_node` atomic, stats bumps, insert into scratch `batch.nodes` HashMap
+ `created_nodes` HashSet, `push_redo` → `post_to_indexes` (lazy: only already-built pairs, so a pure
CREATE pays 1 HashMap miss per property), append to compact `RedoLog`, `shift_workspace`.

**Per-CREATE-row, apply** (`graph/entity_mutation.rs install_node`): `catalog.lock()` Mutex +
`record_labels` **cloning label Strings per row**; `LiveNode::from_parts` (interning + BTreeMap→CompactProps);
logical accounting; `count_stats` Mutex bump; constraint-key seams; `nodes.entry(id).or_default().push(...)`
+ `fit_reserved` (capacity re-fit per row); **`index_add`** (re-reads the node from the DashMap,
`label_index_insert` per label — EAGER — then `declared_dirty_pairs` = (labels+1) × props × 4 DashMap
lookups); `mark_node_dirty` (Mutex<HashSet>); **`string_dirty_mark_node` repeats the whole
`declared_dirty_pairs` walk a second time**. `install_rel` adds 2 adjacency DashMap inserts + degree
histogram + rel stats.

**Bulk batching exists but bails on constraints** (ADR-0209): `relate/relate/src/commit/bulk_batch.rs`
`BulkBatchPlan::prepare` with `DEFAULT_BATCH_THRESHOLD = 10_000` / `DEFAULT_BATCH_BUDGET = 25_000`
(`exec/bulk.rs:53`), but `collect_targets` (`exec/bulk/run.rs:134`) **falls back to the single-commit
path if ANY constraint exists** — which is exactly the merge/update/delete lanes, so their 50k/100k
statements materialize scratch + redo + encoded WAL payload in one commit.

**Commit** (`relate/relate/src/commit.rs`): group commit; leader takes publication WRITE lock + WAL
Mutex, preflights (`preflight_record` **JSON-encodes each record once, proof owns the payload**),
**one `sync()` fsync for the whole group**, then `graph.apply_redo` per record under the publication
write lock. Coexisting allocations per bulk commit: scratch entity maps + compact RedoLog +
final `Vec<RedoOp>` (~160 B/op) + the group's encoded WAL payloads for every slot + every waiter's own
redo Vec. Value postings are not written at commit — only dirty-marked.

**Open / recovery** (`relate-storage/src/open.rs`, `snapshot/native/mod.rs`, `codec/parquet.rs`):
per manifest part the **whole shard is read into a `Vec<u8>`** (256 MiB shard target, 1 GiB cap, 2 GiB
decoded envelope); Arrow row groups of 64; **per row a `serde_json` parse + `BTreeMap` → CompactProps**
(double representation). Per-row restore (`graph/restore.rs load_checkpoint_node`) is deliberately
minimal — **no index/adjacency work at all** (plus a `catalog.lock()` per row). Then `finish_restore`
does the single deferred build: `rebuild_adj` (collect **all** live rels into a Vec, sort, rebuild both
adjacency maps, degree stats, count stats) + `rebuild_indexes` (`Indexes::clear()`, label pass over
`all_node_ids_at` **calling `node_at` per node — full property BTreeMap materialization**, constraint
keys, `rebuild_declared_indexes` = one full label-bucket scan **per declared index**, `recompute_used`).
Then **WAL replay goes through the normal eager per-op seams** (`apply_replayed_redo`), and then
**`graph.rebuild_indexes()` runs again unconditionally** (`open.rs:346`) — a second wholesale rebuild
even when the WAL had zero records past the checkpoint. All single-threaded.

### B. Memory format + index machinery

**Store**: `nodes: CoreMap<NodeId, Vec<NodeVer>>`, `rels: CoreMap<RelId, Vec<RelVer>>`
(`graph/mod.rs:303-306`) — DashMap of heap `Vec` version chains, not SoA/arena. `Ver<Live> { xmin, xmax, live }`.
`LiveNode { labels: Vec<u32>, props: CompactProps }`; `CompactProps { keys: Vec<u32>, vals: Vec<Value> }`
(two parallel vectors, linear `slot_of` scan); `RelPayload::Triple` fast shape for
`{id, amount, ts}`, else `Other(CompactProps)`. Interner is append-only, never frees. Every vector is
capacity-fitted to next-pow2 on every push (`memory.rs fit_reserved`).

**Eager vs lazy today**:
- **EAGER per write**: label index (`label_index: DashMap<u32, HashSet<NodeId>>`), count stats (4 maps),
  degree histograms (`[u64; 21]` per (label,type), high-water), adjacency (2 hashmaps), constraint-key
  tuples (ADR-0439), dirty sets, interner, redo clone.
- **ALREADY LAZY**: equality postings (`prop_pairs`/`rel_prop_pairs`, ADR-0063/0185), TEXT/RANGE/POINT
  declared indexes — built on first seek / at DDL, then per write only a dirty-set insert, drained at
  seek (`declared/mod.rs:201`). `GramIndex` even has a bulk-build mode (`bulk: true` until `seal()`).
- **ALREADY DEFERRED**: checkpoint restore (`load_checkpoint_*` touches no index; `finish_restore` is
  the single bulk pass).

So the deferral machinery the user is asking for **already exists and is proven on the restore path**;
the gap is that the *write* path (WAL replay, bulk CREATE) has no deferred mode, and the restore
path's own final passes have avoidable transient spikes.

**Single-version overhead**: even an entity that is never edited pays one DashMap slot + a heap
`Vec<NodeVer>` of capacity 1 (~88 B node / ~104 B rel) + 16 B of stamps; charge model
`chain_bytes(1) = 24 + 24 + stride` (`memory.rs:81-95`). Every read walks the chain
(`visible_version`, reverse scan) even at len 1. A restored store is *exactly* the all-single-version
case (`xmin = 1`). Blast radius of a "drop the chain when len == 1" change is localized to ~8 sites
(`visible_version`/`live_version`, `cow`, `abort_write`, `gc_*_chain`, `restore`, the two pair-value
walks, the charge walks).

**Adjacency**: `TypedAdj { chunks: Vec<(u32 type_id, Vec<AdjHop>)> }`, `AdjHop { ts: Option<i64>, id }`
= 24 B/hop, ts-DESC (ADR-0035/0203), insert = `binary_search` + `Vec::insert` shift, lists are
supersets verified per hop at traversal, **every rel stored twice** (out + in). CSR would replace the
per-node Vec-of-Vec-of-hops; the ts-DESC order and the superset/visibility contract are the semantic
constraints any replacement must keep.

**Parallelism surface**: execution is concurrent (private `WriteScratch` per connection); **apply is
serialized** — publication WRITE lock + WAL Mutex + group leader + `Graph::in_write` AtomicBool, plus
per-row global Mutexes (`catalog`, `theater`, `count_stats`, `dirty_nodes`, constraint-key map).
The containers (DashMaps) would tolerate concurrent disjoint inserts; the choreography and the per-row
mutexes are the barrier.


## Profiling runs

Machine: Mac16,6 (M4 Max), 16 cores, 128 GB, macOS 27.0.1. Build: `--profile profiling`
(debug info + release opts). Dataset: LDBC `snb-interactive-v1` SF0.1 CSV, **97 MB extracted**,
**327,588 nodes / 1,477,965 rels**. Time slot claimed with peer sessions first (relate-fa confirmed free).

**Dataset provenance caveat:** every run in this document used the local
`CsvBasic-LongDateFormatter` extraction
(`/Users/mh/d/data/ldbc-dbrx/x/social_network-sf0.1-CsvBasic-LongDateFormatter`), **not the
suite's pinned `CsvBasic-StringDateFormatter`** (`ldbc.sha256`). The date columns are
epoch-millis instead of ISO strings, which changes cell widths and therefore row bytes —
absolute RSS and wall numbers shift a little versus a pinned-layout run, and any number from
here cited beside `REPORT-sf0.1.md` must carry this flag. The *ratios* the conclusions rest on
(restore vs build, prep vs write, single vs concurrent) compare like against like and are
unaffected.

| Path | Peak RSS | Steady RSS | Wall |
| --- | --- | --- | --- |
| A. CSV → `:memory:` (loader, `BATCH_ROWS = 5000`, upstream indexes) | 1.01 GB | 1.01 GB (0.93 GB at teardown) | 27.0 s load |
| B. CSV → store + checkpoint (119 MB on disk) | 1.15 GB | — | 32.0 s |
| C. Restore from that store (read-only reuse) | **1.87 GB** | **1.75 GB**, flat for 25 s | 5.7 s |
| D. `bulk-loadtest --dir :memory:`, 100k rows/statement | 419 MiB | — | 5.85 s / 660k rows |

RSS sampled every 0.25–1 s via `ps -o rss=` during each run; peaks cross-checked with
`/usr/bin/time -l` (`maximum resident set size`).

**The finding: restore-from-checkpoint costs ~73% more resident memory than building the same
graph from CSV, and the extra is retained, not transient.**

- C steady = 1.75 GB vs A steady = 1.01 GB → **+0.74 GB for an identical logical graph**.
- C peak / store size = **15.7×** (1.87 GB from a 119 MB store); ~**1 KB/entity** resident.
- The 1.75 GB is flat for 25 s of query looping after open — freed restore transients are not
  returned to the OS (macOS malloc-zone retention), so this is a resident-memory regression, not
  a spike that decays.
- **SF1 measured (2026-11-07, profiling build, pinned StringDateFormatter dataset, contended
  machine — wall times inflated, RSS clean):** store = **630 MB** on disk (3.18M nodes /
  17.26M rels); **build-from-CSV peak 10.74 GB** (702 s wall, contended); **restore peak
  21.25 GB, steady 18.7 GB flat** (111 s wall, contended; peak from `time -l`, steady from 1 s
  `ps` polling during a 61k-round query loop). The earlier ~12 GB extrapolation was an
  **underestimate**: restore peak is 2.0x the build peak (SF0.1: 1.6x), and restore/store grows
  15.7x -> 33.7x — **the restore overhead scales super-linearly with store size**. At SF1 an
  open alone needs a ~24 GB machine; the documented 12 GB figure in `docs/datasets.md` matches
  the build peak (this run: 10.74 GB), not the restore peak. Store kept at
  `/tmp/relate-sf1-store` for re-measurement after S1/S6 land.
- SF1 extrapolation (11.4× the entities) ≈ 12 GB — reproduces the 12 GB figure in `docs/datasets.md`.
- The load path (A/B) never holds that memory: it allocates the graph incrementally per 5k-row
  batch, so peak ≈ graph + one batch. Restore allocates per-row decode transients (whole-shard
  `Vec<u8>`, Arrow batches, `serde_json::Value` tree + `BTreeMap` → `CompactProps`) *and* then runs
  two wholesale rebuild passes over the finished graph.

**CPU self-time, load phase** (macOS `sample`, 1 ms, symbolized; `samply` artifact also produced —
see below). Top entries, ~24 s window:

| Bucket | Samples | What it is |
| --- | --- | --- |
| allocator (`xzm_free/malloc/realloc`, `malloc_zone_malloc`) | ~2,500 | per-row alloc/free churn |
| `Interner::get` + `sip::Hasher::write` + `hash_one` | ~1,700 | string interning via SipHash — the largest engine leaf |
| `platform_memmove` + `platform_memcmp` | ~1,180 | Vec growth, key compares |
| `classify_prop`, `node_account`, `structure_bytes`, `add_logical_bytes` | ~730 | per-row logical memory accounting |
| `insert_hop` (binary_search + `Vec::insert`) | branch counts small (verified path, unranked) | adjacency insert, 2 hops/rel |
| `BTreeMap::insert/drop`, `PropertyMap::from`, `CompactProps::to_map` | ~310 | property-map materialization |
| `from_utf8` + `Utf8Chunks::next` | ~300 | UTF-8 validation on the hot path |
| `serde_json::format_escaped_str` | 110 | WAL payload JSON encoding during load |
| `gc_versions` | 283 | **version GC running during the load** |

Note: `samply`'s `--save-only` profile is unsymbolicated without the Firefox-Profiler UI, so the
numeric attribution above comes from macOS `sample` (same sampler class, symbolized Rust frames);
the `samply` recordings were produced as openable artifacts:
`/tmp/relate-ldbc-sf01-load.json.gz`, `/tmp/relate-ldbc-sf01-restore.json.gz`.

**Symbolication caveat (applies to the table above).** The counts are from `sample`'s
"Sort by top of stack" section, i.e. **leaf** attribution — where each sample landed — and those
leaves are trustworthy. Anything built from *caller* chains is not: frame-pointer unwinding fails
inside the allocator, so the frames recorded above an allocator leaf are garbage that still resolves
to plausible-looking symbols. Two consequences for this document:
- The `insert_hop` row was first written as "top inclusive engine frame" from an inclusive count; that
  count was a parse artifact (a symbol byte offset read as a sample count). The call path itself is
  verified coherent in the raw profile (`load_rels` closure → `TypedAdj::insert` → `insert_hop`,
  `store.rs:702-705`), so adjacency insert is real work in the load — it is just **not ranked** by
  this data. Treat any "hottest function" claim from the call graph as unverified until full stacks
  are read as call paths (the allocator-adjacent region is where this bites).
- The allocator rows (`xzm_*`, `malloc_zone_malloc`) are genuine leaves but say nothing about *what*
  allocated; the "per-row alloc/free churn" reading is an inference from the code map, not from the
  profile. Build with debug info (the `profiling` profile has it — line numbers resolved) and read
  whole stacks before believing a caller chain.

**Loader shape** (`testing/relate-query-latency/src/load.rs`): `BATCH_ROWS = 5000`, one autocommit
`UNWIND $rows AS r CREATE (n:Label) SET n = r RETURN ...` per batch; relationships load as
`UNWIND $rows AS r MATCH (a) WHERE elementId(a) = r.s MATCH (b) WHERE elementId(b) = r.e CREATE ...`
— i.e. **two elementId resolutions per relationship** (1.48M rels → ~3M endpoint lookups;
`parse_element_id` shows up in the profile).


## Prep-vs-write split and concurrent loaders (2026-11-07, follow-up measurement)

Throwaway diagnostic (env-gated prep-skip in the loader + scratch bin; reverted after measuring):
same SF0.1 dataset (LongDateFormatter extraction — see the provenance caveat above), `profiling`
build, `:memory:`, upstream index profile.

| Run | nodes | relationships | total |
| --- | --- | --- | --- |
| Full load (1 conn) | 4.76 s | 16.51 s | 23.18 s (25.0 s wall) |
| Prep only (no statements): node files | 0.34 s | — | — |
| Prep only: rel files (nodes written) | 4.49 s (write) | 0.64 s (prep) | — |
| Rel files on 4 concurrent connections | 4.62 s | **10.88 s** | — |
| Rel files on 8 concurrent connections | 4.81 s | **8.81 s** | — |

- **Source prep (CSV read/parse/row build) is ~4% of load wall** (0.98 s of 23.2 s). The engine
  write is 96%, and within it relationships dominate (16.5 s vs 4.8 s nodes — two endpoint
  resolutions per rel plus the per-row eager seams).
- **Concurrent loaders help today**: 4 threads = 1.52×, 8 = 1.87× on the rel phase, but the
  speedup saturates ≈ 2× — about half the rel-load cost sits inside the serialized
  `apply_redo` (interning, accounting, adjacency, WAL encode), which no number of loader
  threads can touch. Amdahl-consistent: ~50% serial fraction.
- Memory cost of 8 concurrent loaders: peak RSS 1.16 GB vs 1.04 GB single (+0.12 GB scratch).
- Implication for staging: harness concurrency is a cheap, small win now (files are
  independent given node ids; the `run_workers` pattern already exists), but the ceiling is
  the serialized apply — S2's deferred bulk apply is what raises it, and S3's parallel
  transform/build composes on top. Measurement method note: prep-skip runs still pay the
  full CSV read and `Value` construction, so "prep" here is the honest upper bound of what
  harness-side parallelism can recover on the source side.

## Cross-check: codec comparison (parallel session, contended run, 2026-11-07)

An independent codec comparison on this machine (synthetic fraud graph, scale 2 = 30k nodes /
8.7M rels, four snapshot codecs, checkpoint + load wall + store bytes) reports:

| codec | store | vs parquet | checkpoint | load |
| --- | --- | --- | --- | --- |
| parquet | 353 MB | 1.00× | 27.8 s | 21.8 s |
| json | 1169 MB | 3.31× | 11.9 s | 20.1 s |
| rkyv | 1076 MB | 3.05× | 9.5 s | 20.7 s |
| fjall | 368 MB | 1.04× | 31.8 s | 21.1 s |

Load avg 8 during the run, so timings are indicative, sizes valid. The load-relevant
conclusion: **all four loads land at 20–22 s regardless of codec** — including JSON, which
pays the full parse, and rkyv, which pays none. Decode is therefore **not** the load wall;
graph construction is. This corroborates the prep-vs-write split above from the other
direction, and it bounds what a property-encoding change can claim: memory and size, not
load wall-clock. Restore-memory measurements, not wall-clock, are the acceptance for any
such change.
