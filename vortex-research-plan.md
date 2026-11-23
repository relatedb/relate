# REL8-1982 — Vortex in-memory columnar property store (round 1)

Branch `perf/rel8-1982-vortex-lab`, worktree `relate-wt/vortex-lab`.
MSRV is lifted locally on this branch (lab-only; vortex 0.93.0 needs rustc 1.95,
local toolchain 1.100). The repo's 1.88 pin governs only what could merge —
nothing here merges into a shipped crate; the MSRV question is ADR material.

## Steps

- 10 DONE-ish Track: REL8-1982 rewritten vortex-only; REL8-2004 (Lance) filed Backlog
- 20 DONE Skeleton: `testing/relate-vortex-lab` crate (codec-lab precedent),
  vortex 0.93.0 `default-features=false features=["files","memmap2"]`,
  smoke PASSED (c6c7f096): 1M sorted u64 keys, blocking write over std
  io::Write, Mmap -> ByteBuffer -> open_buffer -> scan, 3380 B vs 8.4 MB raw.
  API notes: VortexSession::default().with_handle(rt.handle()) required;
  sync = SingleThreadRuntime + BlockingRuntime::block_on; write via
  write_options().blocking(&rt).write(sink, array.to_array_iterator())
- 30 DONE Size bench: bytes/property vortex vs CompactProps vs parquet
  checkpoint, shapes ring + flat first, then the rest + LDBC SF0.1
- 40 DONE (bench side) Seek bench, two-construct design (user 2026-11-23):
  node-major store (rows sorted by dense node-id: node-id -> props is
  direct indexing) + per promoted key EITHER
  A: separate (key, node_id) key-sorted index columns — unique and
     non-unique unified, key runs contiguous, node-id secondary sort
     delta-friendly, ~8-14 B/node vs postings' 213 B/node
  B: dictionary-encoded key column inside the node-major store
     (DictArray dictionary + codes = compressed postings, no second file)
  both arms vs the post-#672 compact postings: ns/lookup, build/seal
  cost, resident bytes; also retire the id-vec dense slots (~325 B/node)
- 45 DONE Operator kernels O1-O20 (spec in workload-mapping-findings.md):
  second sweep added O12-O18 (prefix range, countDistinct-via-dict,
  histogram/quantiles, order/top-k, presence/label-prune, anti-join,
  IN-list) and O19-O20 (traversal set state: seen/frontier as roaring
  row-sets, ~bits/node vs HashSet's 48+ B/node, order-preservation check;
  distinct over packed dict code pairs)
  O11 = row-position-carrying operators: row==node-id identity, aligned
  gather-compares (n.age<m.age), row-set aggregates as compare bounds
  (stats fast path when chunk-aligned), position-set algebra for
  conjunctive filters; global row space (sparse per-label columns) pinned;
  O9-locality vs identity tension benched;
  point read by node-id, multi-column gather, key seek, range->count/exists,
  conjunctive filter, group-by/top-k over dictionary, property-key merge
  join, intra-row compare, traversal-order reads, AND the morsel-batch
  operators (O10): 1024-key batched check (is_in over key column/dict) +
  batched predicate multi-check (take-gather + compare -> 1024-bit mask),
  both seek strategies benched (1024 binary seeks vs sort+merge, order
  restoration cost included).
  Unit = operator, metrics =
  bytes decoded, allocs, ns. Stats pushdown is a given for every promoted
  column (user 2026-11-23: the residual column's lack of per-key stats does
  NOT weaken the story — promotion threshold is the dial, bench measures
  where it sits; residual per-key aggregates at seal = optional probe)
- 50 DONE Filter/scan bench: equality/range through chunk stats,
  compute-on-compressed vs decode-then-filter; map 30 query-latency queries
  to property-access patterns; conjunctive shape (16/30 are name=X AND
  name=Y); residual-keys probe: per-key min/max attached at seal via
  with_file_statistics/with_metadata_segments vs promotion threshold vs
  sidecar (default: residual column stats are per-column, NOT per business
  key — no per-key pruning without one of the three)
- 60 DONE VFS mmap prototype (ops/vfs.rs, 877b46ee+): struct vs per-column
  layouts — reads layout-neutral, opens are not (~1.7k allocs/footer ->
  lazy open); remap cost; page-skipping measured 42x (16 KB vs 672 KB).
  NOTE: engine integration loads through the regular VFS APIs (user
  2026-11-23) — no new I/O seam with the new store; vortex is an
  experimental codec-shaped option (ADR-0665). Parquet-decode-on-open
  comparison still open (needs the engine codec path).
- 70 TODO P1 Measurements in the linux/amd64 container harness (learnings
  recipe: 2 vCPU/2 GB, smaps sidecar, dhat behind a throwaway feature)
- 80 HALF vortex-research-results.md tables done; ADR-0665 (Proposed) written 2026-11-23 — (Proposed; adopt /
  adopt-for-checkpoint-only / drop; MSRV finding: 0.43 vs 0.93 delta)
- 70 TODO P1 (unchanged) container-harness measurements
- 85 HALF (2026-11-23) seam designed + lab-validated: PropStore trait
  (seam.rs) with RowProps/ColProps impls, A/B measured (expand 20x/74x
  allocs trade, batch 3.2x, topk 4x, seek parity, naive-distinct parity
  -> dict rung required); engine-API mapping table in vortex-research-results.md +
  exec-surface-findings.md. REMAINING: engine-side insertion at
  node_prop_id_at + candidates funnel (lookup_nodes_upto_at) +
  stats_short_circuit rung; CompactProps stays default behind the trait
- 90 TODO P1 Draft PR (lab crate + RESULTS + ADR; stellar full round pre-push)

## Facts pinned so far

- vortex 0.93.0: rust-version 1.95, Apache-2.0, `memmap2` feature, zstd optional
- vortex 0.43.0 = newest at MSRV 1.88 (fallback if maintainers rule no MSRV bump)
- baseline to beat: lazy postings ~213 B/node ring (9.8 MB / 46k nodes);
  CompactProps ~48 B/node incl. values
- wide layout to mirror: `relate/relate-storage/src/codec/parquet/wide.rs`
  (ColumnType Bool/Int/Float/Str + residual tagged-JSON)
- VFS seam: `relate/relate-storage/src/vfs/mod.rs:250`

## Vortex fork patches + upstream PRs (workstream, 2026-11-23, user-directed)

Decisions: label filter = per-label dense bitset in the store (adopted);
aggregation must be vortex-native; monkey-patch locally until upstream
fixes; propose PRs upstream for the patches (open only on user go).

- 90P DONE Fork setup: fork vortex-data/vortex, checkout the 0.93.x
  line, lab pins it via [patch.crates-io] (MSRV-lift is already sanctioned
  for the lab). Patches land on the fork; each patch = one upstream PR.
- 91P HALF Patch P1 — with_limit early exit (#11808): knob measured on
  the fork (max_splits_in_flight=1: exists -43% time / -33% allocs;
  capped expand 45% vs uncapped); colstore applies it under a cap
  automatically. REMAINING escalation: bound IO read-ahead under a limit
  (the uncancelled spawns) — or wait for upstream lazy scheduling
  (#11808 reopened as its tracking issue): the budget-stop
  EXISTS (vortex-layout scan/layout.rs:249/292 checks RowLimit::
  is_exhausted) but a filtered limit(1) still pays ~98% of a full scan
  (measured O4c: 734 allocs vs 706 full) — root cause to pin on the fork:
  in-flight concurrent splits are not cancelled once the budget empties
  (all splits' decode+filter paid before first emission), or the
  VortexFile::scan driver path never consults the budget. Patch: cancel/
  drop pending split futures at exhaustion; gate split concurrency when a
  limit exists.
- 92P TODO P0 Patch P2 — native aggregation: vortex-compute has NO
  value_counts / count_distinct (only lane kernels: collect_bits, maps,
  sink/source). Add value_counts(array) -> {values, counts} +
  count_distinct(array) -> u64, dict-native (public Dict values()+codes();
  the lab's O13b2 technique: 22us vs 27ms), hash fallback for primitives.
  In the lab, the seam's distinct switches to this path.
- 93P TODO P1 Upstream PRs: P1 + P2 against their repo (their norms:
  check CONTRIBUTING first); only on explicit user go.
- 94P TODO P0 Lab seam completion queue: Val-typed Pred (string leaves,
  In, IsNull/NotNull), string point/batch via the typed decoded cache
  (canonicalize VarBin, per-value scalar_at; execute_scalar is the
  measured public probe), per-label bitset for expand, prefix_range
  (O12), nullable null_count.

## Round 2 (2026-11-24): engine integration + report-only lane

- 95P DONE — report-only CI lane `.github/workflows/vortex-lab.yml` (1.95 toolchain via RUSTUP_TOOLCHAIN env; lab crate workspace-excluded with own Cargo.lock). First green run 45290311761; numbers posted to REL8-1982. Fixes: toolchain-pin override, nullglob publish, /proc/loadavg stamp, stdio-only artifact.
- 96P RUNNING — gbuild feature `vortex-engine-integration` (branch perf/vortex-engine-integration, stacked on this branch; store at .git/gbuild/vortex-engine-integration/db). 7 nodes/4 waves: wave 1 = propstore-trait, msrv-gate-probe, lab-memory-kernels (dispatched); then mmap-accounting-adr + vortex-checkpoint-codec, vortex-propstore-impl, engine-ab-eval. `/gbuild:run vortex-engine-integration` resumes.
- Pokec for the lane: in-repo synthetic fixture testing/relate-test-support/src/pokec.rs (MEDIUM = 100k/1.77M); the private-repo pokec lane (REL8-1846) is Canceled.
