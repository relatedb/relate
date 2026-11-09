# Summary — lazy indexes / load memory (LABS-1110 direction)

Branch `research/lazy-indexes` (worktree `/Users/mh/d/rust/relate-wt/lazy-indexes`), base `origin/main`
d50ad39, 2026-11-07. Research + proposal only; no code changed.

**Documents**: [`findings`](lazy-indexes-findings.md) (evidence + code map) ·
[`analysis`](lazy-indexes-analysis.md) (why the numbers are what they are) ·
[`proposal`](lazy-indexes-proposal.md) (the staged plan).

## Headline

**Restoring a store costs 73% more resident memory than building the same graph from CSV, and the
extra is retained, not transient.** LDBC SF0.1 (97 MB CSV, 327,588 nodes + 1,477,965 rels, M4 Max,
`profiling` build):

| Path | Peak | Steady |
| --- | --- | --- |
| CSV → `:memory:` | 1.01 GB | 1.01 GB |
| CSV → store + checkpoint (119 MB) | 1.15 GB | — |
| **Restore from that store** | **1.87 GB** | **1.75 GB** (flat 25 s) |

15.7× the store size, ~1 KB/entity. Extrapolates to the documented 12 GB SF1 peak.

## What follows from it

1. **The deferral machinery already exists** — value postings are build-on-first-seek (ADR-0063/0185)
   and restore already defers to one `finish_restore` pass. The gap is the bulk write path and WAL
   replay, which still run the transactional eager path per row.
2. **The 0.74 GB is decode shape, not index maintenance** — whole-shard `Vec<u8>`, Arrow batches,
   `serde_json::Value` + `BTreeMap` per row, `node_at` per node in `rebuild_indexes`, an all-rels
   `Vec` + sort in `rebuild_adj`, and a second unconditional `rebuild_indexes()` after replay
   (`relate-storage/src/open.rs:347`).
3. **The durable property encoding is hostile to the memory format** — tagged-JSON string column →
   `serde_json` → `BTreeMap` → `CompactProps`, re-interning keys that were interned at write time.
4. **Load CPU is allocation/interning/accounting bound** (~10% allocator, ~14% interner + SipHash,
   ~10% memmove/memcmp, ~6% memory accounting) — the query front end does not appear.
5. **Store-format work is not the first lever**: single-version chains ≈ 15% of steady state; CSR is
   already rejected in ADR-0206 and these numbers agree. The supported change there is a bulk *append*
   path for `insert_hop`, not CSR.

## Proposal in one line per stage

**S1** fix restore allocation shape (stream shards, one-pass decode, no `node_at` materialization,
streaming `rebuild_adj`, drop the double rebuild) → **S2** deferred apply mode for bulk CREATE + replay
(and unbreak constrained bulk batching) → **S3** parallel transform + parallel final build (commit stays
single-writer; fix the loader's 2 endpoint lookups per rel) → **S4** lazy label index → **S5** cheap
per-row wins (double dirty walk, `fit_reserved` per push, interner hashing, WAL JSON payload) →
**S6** columnar/typed checkpoint shard (own ADR).

## Industry cross-check

Traverse (Truespar) — offline BulkWriter + auto-indexing at the first `MATCH` after the CREATE phase +
memory ceiling + allocator-ratchet fix; Neo4j — background index population and a dedicated import build
stage; Memgraph — three-phase concurrent index creation, parallel schema recovery, and an analytical
import mode that drops MVCC deltas entirely; Kùzù/DuckDB — indexes built after bulk copy. Common shape:
**bulk load is its own path, index construction is its own phase, MVCC overhead is dropped on the bulk
path.**

## Open questions

Order (S1 vs S4 first); whether to open the S6 ADR now; whether a dedicated bulk-load surface is in
scope; whether to run SF1 to confirm the extrapolation.
