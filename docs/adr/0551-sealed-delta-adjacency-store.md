# ADR-0551: Sealed + delta adjacency store — data model and invariants

Status: Proposed
Claim: 4d0ad33b50986d6984e6fdc7b31739a7642e0155

## Context

The compressed-topology ADR (ADR-0206) conditionally adopted compressed
committed adjacency chunks: its hybrid codec saves 95.89–99.31% of
topology-twin bytes with scalar decode 33x–173x off the live expansion
critical path, but compression is append-hostile, so adoption was
conditional on a transactional delta-store design. This ADR is that
design. Each (node, rel type, direction) chunk is today one uncompressed
sorted hop vector (16 B/hop, ts-DESC insert) plus the exec/overlay
pending-write adjacency merged at read; checkpoints are the one moment
the store may pay O(chunk) reorganization. The design and its prototype
were completed in the project's pre-rename home and are ported here for
the record.

## Decision

**Three tiers per (node, rel type, direction) chunk.** The in-flight
exec/overlay pending-write adjacency, merged at read with per-transaction
visibility, is unchanged. A committed delta region materializes the
overlay's operations on commit as the net effect per touched rel id
(last-write-wins, not an op log) into a per-chunk delta vector keeping
today's sorted ts-DESC insert semantics, plus a per-chunk tombstone set. A
sealed region holds compressed immutable chunks in the ADR-0206 hybrid
codec; the ts-DESC chunk order (ADR-0035) and newest-first expansion
(ADR-0203) stay.

**Read merge.** Delta first, then the sealed stream; both are ts-DESC and
fresh hops are almost always newer, so the common case is
drain-delta-then-decode-sealed. Sealed hops whose id is in the chunk's
tombstone set are filtered by snapshot visibility: tombstones carry an
xmax version like today's version chains, so an older snapshot still sees
the sealed hop.

**Write path.** Insert and reposition touch only the delta, at today's
sorted-insert complexity. Delete removes from the delta (if present) and
adds a versioned tombstone for the sealed hop; a timestamp update of a
sealed rel is a tombstone of the old hop plus a delta hop with the new ts.

**Compaction and watermark.** At checkpoint, fold delta and tombstones
into the sealed stream, re-encode, and drop both. Compaction also fires
early at a delta-size watermark so the merge region cannot grow unbounded
between checkpoints, and must be confluent (identical final sealed bytes
regardless of when it fired) and idempotent. The gate is per chunk: a due
chunk does not drag its sibling chunks of the same node into the fold. A
fold covers exactly the chunks where the delta exceeds 25% of the sealed
hops, or the chunk was never sealed and its delta reaches a floor of 32
hops, or the sealed region carries a tombstone reclaimable at the
garbage-collection horizon (a superseded position, or a deleted row whose
tombstone version is at or below it). The floor of 32 is the measured knee
of a cutoff sweep over the 17 example datasets (final-wave measurement,
per-chunk degree axis):

- cutoff 32 seals 14,850 chunks (9.1 MB hybrid) for 48.7% aggregate
  hop-weighted coverage, versus 37.0% at cutoff 1024; legis-graph moves
  7.4% to 81.5%;
- cutoff 16 adds 71% more sealed chunks for only +0.88pt coverage at
  3.64 B/hop — past the knee;
- the 8..15-hop bucket already decodes at 119–172 Mhops/s and 2.3–4.1
  B/hop, and the worst sealed decode (~120 Mhops/s) is still ~80–100x the
  live expansion baseline, so decode never binds;
- sub-8-hop chunks are pure header overhead.

A fully reclaimed chunk loses its sealed region entirely, so the layout is
independent of when compaction fired; an empty delta with no reclaimable
tombstone is a true no-op. The floor only affects a never-sealed chunk's
first seal.

**Invariants.** Rel and node ids are monotonically allocated and never
reused, so tombstones can be bare ids; a recycled id would alias a live
rel to a dead one's tombstone and silently hide live adjacency (it would
need id and epoch pairs). Tombstones stay versioned because a snapshot
taken before a delete must still see the sealed hop. Net-effect commit
bounds delta memory by distinct touched rels since the last compaction,
not by operation count.

**Accounting and codec source.** Sealed chunks are charged at compressed
size and the delta at live size, per ADR-0042 as amended by ADR-0202; the
memory-shape loadtest baselines need re-freezing through ADR-0071's CI
dispatch, never by hand. The hybrid codec moves from the `relate-datasets`
testing crate into `relate-executor` as a new adjacency codec module —
engine crates depend only on engine crates (ADR-0103 rule R1) and the move
adds no new external dependency. The experiment crate keeps its own pinned
copy, since a testing crate cannot import engine-internal modules; a
shared home in `relate-core` is rejected for now.

## Alternatives considered

- **Rebuild-on-write per adjacency list.** Rejected: O(degree) per write
  to hub nodes, the redo-path regression ADR-0206 warned against.
- **Raw-overflow slack inside compressed blocks.** Rejected: per-block
  fragmentation bookkeeping in a footprint-first engine.
- **CSR as the primary store.** Rejected: batch-built CSR fights the redo
  path; checkpoint-rebuilt read-only CSR stays a future acceleration
  structure only.
- **Uncompressed-only delta, no sealed region.** Rejected: no memory win —
  ADR-0206's 95.89–99.31% saving is the sealed form.
- **Elias-Fano for the id stream.** Rejected: decode too slow for
  transactional latency.
- **SIMD decode.** Rejected: the NEON block decoder does not beat scalar
  (0.72–1.07x), which already runs far off the critical path.
- **Sealing floor on per-node total degree.** Rejected: adds only
  +0.7–1.4pt coverage for 2–4.3x the sealed chunks — small chunks whose
  headers cost more than their payloads save.
- **Seal everything at stability.** Rejected: 100% coverage but 8–13x the
  bytes of the floor-32 hybrid; sub-8-hop chunks are pure header overhead.
- **Floor below 32 (16 or 8).** Rejected: cutoff 16 buys +0.88pt coverage
  for +71% chunks; the knee is 32.

## Consequences

- The overlay tier and its tests carry over unchanged. Read latency is
  drain-a-small-delta then decode-a-sealed-stream, bounded in the worst
  case by the watermark policy and checkpoint cadence. Write complexity is
  unchanged; only compaction pays the O(chunk) re-encode, off the write
  path. Any future id-recycling proposal must land a new ADR replacing
  bare-id tombstones with id and epoch pairs.
- Snapshots and WAL replay do not serialize the sealed+delta layout:
  checkpoints snapshot node and rel rows at the snapshot horizon and
  restore rebuilds adjacency from those rows as a fresh delta, so sealed
  bytes and tombstones are a derivable in-memory layout, not durable
  state. A reloaded store is delta-only and the next checkpoint re-seals
  it; the dump horizon is the durability floor. Durable sealed bytes would
  be a snapshot-codec change and its own ADR.
- The codec exists in two places on purpose; the experiment copy stays
  pinned while ADR-0206's numbers are the reference evidence. Baselines
  under `baselines/` are re-frozen via ADR-0071's CI dispatch; local
  baseline runs on the branch are not gate-comparable until then
  (ADR-0070).
- The existing memory shapes do not model mid-degree graphs (flat 0, ring
  1, dense-ring ~20, fanout leaves 1 hop per chunk — all below the 32-hop
  floor; only the fanout hubs seal, as they already did at 1024), so their
  RSS envelopes are unchanged by the floor move. The measured win is
  example-dataset coverage (48.7% vs 37.0%); a mid-degree memory shape
  with per-node degree straddling 32 is the follow-up that would show the
  floor's payoff in the shape gate.
