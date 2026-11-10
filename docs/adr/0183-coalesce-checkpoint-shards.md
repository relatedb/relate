# 0183. Checkpoint rewrites coalesce neighboring shards up to the byte target

- **Status**: Proposed
- **Date**: 2026-10-07
- **Deciders**: CypherLite maintainers

## Context

ADR-0022 checkpoints copy-on-write id-window shards: only parts that
overlap dirty ids are rewritten, and a part splits when it outgrows the
byte target. Its Decision 4 left "merge of undersized neighbors" as
follow-on, so nothing ever puts small shards back together.

PR #195 (commit `746b5b72`, "stream checkpoint shards and copy clean
rows") made a rewrite copy clean rows out of the previous shard file
instead of re-encoding them. `rewrite_overlapped`
(`cypherlite-storage/src/snapshot/native/mod.rs`) splits a dirty part's
ids into alternating dirty and clean runs and writes **one shard per
run**: `write_leaf` per clean run (via `copy_id_runs`), `write_split` per
dirty run. A part with k dirty runs becomes up to 2k + 1 parts, and every
later checkpoint that scatters dirty ids over them fragments them further.
ADR-0180 records #195 ("Checkpoint shards stream, split on compact bytes,
and keep clean rows"); its Decision 3 is this one-part-per-clean-run rule.

LABS-194: `cypherlite pull` of the graph-examples recommendations database
(28,863 nodes, 166,261 relationships, large `Movie` properties) fails on
macOS with "Peer disconnected" 60–110 s in. Bisection on release builds:

| Build | Result | Node shard files |
| --- | --- | --- |
| `da2c13a3` (#193, parent of #195) | pulled in 31 s | 1 |
| `746b5b72` (#195) | fails at 70 s | 5,497 |
| `3ecfa775` (main) | fails at 60–111 s | 3,526 after 25 s |
| `3ecfa775` behind a response-buffering proxy | pulled in 204 s | 10,828 |

The pull's relationship statement re-SETs endpoint nodes it already loaded
(the loader reuse path in `surfaces/cypherlite-load/src/materialize.rs`;
162,407 reused nodes). That scatters dirty node ids, so each mid-pull
WAL-rotation checkpoint (`schedule_checkpoint`) cuts the node shards into
1–5-id files. Every shard costs a file fsync plus a directory fsync
(`reserve_owned_object`; `F_FULLFSYNC` on macOS): 3,704 of 4,133
checkpoint-thread samples sit in `fcntl` under `write_leaf`. While that
checkpoint runs, the next ingest commit waits in `begin_publication_inner`,
the HTTP read loop stalls, and Neo4j drops the stream. v0.11.5 ships this
fragmentation, and its stores keep it: a fragment is only ever split again.

## Decision

A checkpoint rewrites id ranges as **rewrite units**, not per previous
part, and writes each unit as byte-budgeted files.

1. **Slots and seeds.** Planning runs over **slots**, a type of its own
   (`layout::Slot`), not over manifest records: a **kept** slot is a
   previous manifest part with its file, a **window** an id range with no
   previous file. `ShardPart` stays a plain manifest record. Only kept
   slots supply copy sources and retired paths; a window is never retired
   or written to the manifest. A kept slot that overlaps a dirty id seeds
   a unit, and so does every window: `cover_ids` adds one for uncovered
   ids, including every window of the first sharded checkpoint. Stores
   written by v0.4.0–v0.11.x, before `cover_ids` clipped new windows to
   the gaps between parts, can hold parts whose ranges overlap and that
   both hold an id, and v0.4.0–v0.9.x's fixed-name `write_split` could
   list one part twice in a manifest. The layout step sorts the previous
   parts once, then **replaces** each group of mutually overlapping parts,
   a chain or a part listed twice included, **by one window** over the
   group's union range; the group's files are retired with the
   checkpoint's other replaced parts. Which part holds a shared id's live
   version is unknown, so nothing is copied from the group: its window
   encodes each live row once, whatever the dirty ids. From then on slots
   are sorted and disjoint, `cover_ids` takes them so, and planning and
   writing rely on it. One entry point lays out, plans and writes a kind.
2. **Absorption.** A unit absorbs its immediately adjacent slots on either
   side in id order, clean kept slots included, while three bounds hold.
   The merged id range `[lo, hi)` spans `hi - lo <= 1,000,000` ids
   (`MAX_WINDOW`, ADR-0022's widest window). The unit's kept slots'
   manifest `rows` sum to at most `Kind::page_envelope_rows()` (see Page
   envelope) and their published `bytes` to at most the cap a file is
   written into, `min(target_bytes, 1 GiB)` (Decision 3); both are read
   from the manifest, without a graph walk. And the unit's live **decoded**
   bytes stay `<= target_bytes`. The width and the manifest are checked first,
   so a slot that cannot join is never measured. The manifest bound keeps
   apart files that miss one file together: the two halves of a file the
   envelope fallback halved, for a unit over the page envelope or for
   incompressible rows near the target, both pass the decoded check, so
   without it the next checkpoint touching either half would merge them,
   miss and halve again, two to three whole-unit encodes every such
   checkpoint. Decoded bytes are `Kind::checkpoint_bytes` of each live id
   at the snapshot timestamp, read from the in-memory graph without
   touching a file. One codec accumulator (`DecodedBudget`) adds them up,
   with checked arithmetic and the ADR-0086 row and decoded envelope
   checks. A seed's rows are measured once, in the walk that also cuts it
   (Decision 3). A seed row the row envelope rejects fails the checkpoint;
   a neighbour row it rejects only does not fit, and that neighbour stays
   as it is. Adjacent seeds join one unit when they fit those bounds
   together; a seed already over the target absorbs nothing. The merged
   slots take their union id range. Planning's walk keeps no row budget:
   a unit holds no more live rows than ids, so `MAX_WINDOW`, the per-part
   row bound on load, holds by construction as it did for ADR-0022
   windows, except in a healed window wider than that (Decision 3).
3. **Write.** Planning cuts each unit into files in the walk that measures
   it, and the writer writes those **planned cuts** in order without
   measuring again. A unit within the target is **one file**. A lone seed
   over the target is cut where the next row would pass the target or the
   decoded envelope, so each file is `<= target_bytes` decoded unless it
   is a single over-target row. A seed wider than `MAX_WINDOW` ids (a
   healed window, or a file written from one) absorbs nothing and is
   first cut every `Kind::page_envelope_rows()` live ids, counted on the
   sorted id list, then each piece by bytes: a wide window of tiny rows
   does not need halving after halving. Files tile the unit's range, each
   starting at its first id (the unit's `id_lo` for the first). Each file
   holds, in ascending id order through one `ArrowWriter`, **copied clean
   rows** (Arrow slices from their previous shard files, never
   re-encoded, which keeps #195's copy) and **freshly encoded dirty
   rows**; ids in a window have no previous file and are encoded. One
   copy cursor serves a unit's files in order, and at most one previous
   shard is held at a time. A unit's runs ascend, so the held cursor is
   reused for a later run of the same kept slot; a file halved on an
   envelope miss drops it, since both halves start behind it. A copy
   consumes the cursor and gets it back only when the copy succeeds, so
   a failed cursor is never used again. A unit is **never** written as
   one file per dirty or clean run. A file of more than one row is
   written into a sink capped at `min(target_bytes, 1 GiB)`, the cap
   `with_shard_writer` takes, so an encoded file over the target
   overflows it. That is an **envelope miss**, as are overflowing the
   1 GiB file ceiling and the reader's preflight rejecting the written
   file on another ADR-0086 bound, and every envelope miss keeps the
   existing halving fallback (`split_ids`, at the middle id). A single row
   is capped by the file ceiling alone. Each failure is classified where
   it arises. Everything else is fatal, since a file of fewer rows fails
   the same way: JSON serialization, `RecordBatch` construction, building
   the `ArrowWriter` or closing it without an overflow, a copied batch the
   writer rejects, and every copy-source failure. A fatal error fails the
   checkpoint without halving. A unit with no live ids writes nothing and
   its kept slots are retired, as today; a planned file that would hold no
   rows is a storage error, raised before any halving, and is never
   published or silently dropped.
4. **Untouched parts.** Every kept slot outside every unit is kept
   unchanged: its manifest entry is reused verbatim (ADR-0089
   Decision 1), zero I/O.
5. **No manifest format change.** `format_version`, `Manifest` and
   `ShardPart` keep their fields. A merged range spans at most
   `MAX_WINDOW` ids, like an ADR-0022 window. A healed window (Decision 1)
   can span more, but each of its files holds at most
   `Kind::page_envelope_rows()` rows (Decision 3). Either way a part's
   rows stay within the `read_manifest` bounds (`<= MAX_WINDOW` and `<=`
   its id width), and v0.11.5 opens the new layout.

## Alternatives considered

- **Re-encode whole dirty windows from the live graph** (pre-#195
  `write_split` over the window). Bounds the shard count again. Rejected:
  it re-encodes every clean row of a dirty window, losing #195's copy;
  coalescing keeps the copy and gets the same bound.
- **Merge small shards in a second pass after writing.** Rejected: every
  merged row is written and fsynced twice, and the per-run files, the
  fsync storm LABS-194 measured, are still created first.
- **Size merges by compressed `ShardPart.bytes`.** Already in the
  manifest, no graph walk. Rejected as the size of a unit: it is a
  different unit than the decoded cut, so a merge can exceed the decoded
  target and be re-split by the next checkpoint, and the layout
  oscillates. Decision 2 still reads the published bytes, only as a bound
  beside the decoded one: it can keep apart files the decoded bound would
  merge, never merge files it would not, so every merge still fits the
  decoded target and the cut, and is not re-split.
- **Wider merged ranges with an explicit row cap.** Let a union range
  span more than `MAX_WINDOW` ids and stop absorbing at 1,000,000 live
  rows instead, so sparse ranges also merge across a window's width.
  Rejected: the row cap is a second budget kept beside the decoded bytes
  in planning's walk and in absorption, plus a test-only knob to exercise
  it, since no unit test can afford a million rows. The width bound keeps
  rows `<= MAX_WINDOW` by construction. The page envelope bound this ADR
  does apply is not that budget: it is one constant the codec derives,
  `Kind::page_envelope_rows()`, compared with rows the manifest already
  records (Decision 2) and used as the piece length of a wide seed's
  sorted id list (Decision 3). Neither counts rows in the decoded walk,
  neither widens a merge past `MAX_WINDOW` ids, and tests exercise both
  at the real bound by forging manifest rows or planning over an id list
  the graph does not hold, with no knob.
- **Bound every unit's rows at the page envelope.** Count live rows in
  planning's walk and cut or stop absorbing at
  `Kind::page_envelope_rows()`, so no file misses the page envelope.
  Rejected: that is the row budget above, kept in the decoded walk. A
  unit of at most `MAX_WINDOW` ids that misses the page envelope costs at
  most one extra encode for nodes and two for relationships (see Page
  envelope), and Decision 2 keeps its halves apart afterwards, so only
  the checkpoint that first writes it pays.
- **Plan over blocks of overlapping legacy parts.** Keep each group of
  overlapping parts as one block that planning never splits, and have
  the writer copy nothing in a unit that holds one. Rejected: overlap
  handling then spans `cover_ids`, planning and the writer, and a unit
  holding a block re-encodes the clean neighbours it absorbs instead of
  copying them. Healing each group into one window first leaves a single
  rule after it: slots are disjoint.
- **Plan over `ShardPart`s, with `rows == 0` marking a window.** Reuse
  the manifest record as the planning window, with a made-up fixed-name
  path. Rejected: whether a range has a previous file then hangs on a
  metric, the made-up path can reach the retired list, and the manifest
  record carries a state it never holds when published. A slot type
  makes "no previous file" a variant planning and writing match on.
- **Defer WAL-rotation checkpoints during a pull** (ADR-0158's "one
  checkpoint at the end"). Rejected for this change: unbounded WAL growth
  on long pulls plus a new storage/open knob, and every other write load
  still fragments. Revisit only if a pull still stalls once shards are
  bounded.
- **Skip redundant loader reuse SETs** for nodes already materialized in
  the same pull. Rejected here: a loader semantics change that removes one
  source of scattered dirty ids, not the unbounded layout; separate
  follow-up.
- **Batch directory syncs once per shard directory** (allowed by ADR-0089
  Decision 2). Rejected as the fix: the file count stays unbounded, each
  file still costs its own fsync, and open still reads every fragment. It
  is compatible with this ADR and can follow independently.

## Consequences

- **Bounded layout.** A unit spans `hi - lo <= MAX_WINDOW` ids unless it
  is a healed window (Decision 1), and holds `<= target_bytes` decoded
  unless it is a lone over-target seed, so absorption never merges ranges
  across more than `MAX_WINDOW` ids. The greedy cut and the absorption
  bounds leave two adjacent output parts of a rewritten range apart only
  when merging them would break a bound, except beside a lone
  over-target seed: together they exceed the target in decoded bytes,
  span more than `MAX_WINDOW` ids, or replace files that hold more than
  `E = Kind::page_envelope_rows()` rows or `target_bytes` bytes between
  them. Without such a seed, a rewritten range of `B` decoded bytes over
  `W` ids, whose previous files held `R` rows in `P` bytes, has at most
  `2 * n + 1` parts, where `n` is the sum of `ceil(B / target)`,
  `ceil(W / MAX_WINDOW)`, `ceil(P / target)` and `ceil(R / E)`;
  each over-target seed holds more than the target by itself, so the
  count stays `O(n)`: it follows the data, never the dirty runs. The
  existing halving fallback and a healed window wider than `MAX_WINDOW`
  ids are the only other exceptions; a healed window's files are cut by
  bytes and every `E` rows.
- **Healing.** Fragmented v0.11.5 stores heal on their next write to a
  fragmented range: the seed absorbs the fragments around it up to the
  target. LABS-194's fragments heal: its 28,863 node and 166,261
  relationship ids each span fewer than `MAX_WINDOW` ids, and their files
  hold far less than the target and the page envelope between them.
  Small parts whose union would span more than `MAX_WINDOW` ids stay
  separate, the cost of the width bound. Untouched fragmented ranges stay
  until written; there is no background compaction, and open still reads
  each of their parts. Overlapping parts left by v0.4.0–v0.11.x are the
  exception: they heal on the next checkpoint of their kind, wherever its
  dirty ids are, into disjoint files that hold each live id once, and a
  part listed twice has its one file retired. A clean neighbour the
  healed window absorbs is still copied.
- **Halved files stay halved.** A file halved for holding more rows than
  the page envelope is published as two parts whose manifest rows pass
  it together; one halved for encoding past the byte cap, as two whose
  bytes come to about what missed the cap plus a second footer. Decision
  2 does not merge them again: a later write to either half rewrites that
  half alone, measuring, reading and encoding no more than it, and only
  the first write pays the halving. A file halved on another ADR-0086
  bound the manifest does not record, such as a large value spilling into
  more pages, can still be merged and halved again.
- **One-time re-encode after upgrading.** The first checkpoint of a kind
  after upgrading a v0.4.0–v0.11.x store re-encodes every live row of
  each overlap group from the graph instead of copying it. A long chain
  of overlapping legacy parts heals as one window, up to every row of the
  kind, so that checkpoint can cost a re-encode of the whole kind, as the
  first sharded checkpoint does, cut into byte-budgeted files of at most
  `Kind::page_envelope_rows()` rows. It happens once: the healed layout
  is disjoint, and later checkpoints copy from it.
- **Fail closed on open.** A manifest part whose file breaks the shard
  rules fails `open_store`, naming the part. The rules are the shard
  schema of its kind, non-null ids strictly ascending through the file,
  so none repeats, and every id inside the part's `[id_lo, id_hi)`. The
  range is one of the rules because coalescing retires a part by its
  range, so a row outside it would be lost or loaded twice. An `i64::MAX`
  row id, which no part range holds since `id_hi` is exclusive, fails by
  the range rule before the loader's `validate_persisted_id` sees it.
  The checkpoint's copy cursor holds a previous shard to the same rules
  over the same range, so open and checkpoint reject a file by one rule,
  and a store that opens is one whose parts a checkpoint can copy from; a
  clean row missing from a valid part is the one copy-source failure open
  cannot see, and it fails only at checkpoint. A format <= 2 monolith is
  exempt from the shard rules: its writer predates v0.4.0, the oldest
  known to keep them, and no checkpoint copies from it. It keeps the
  loader's column-type and persisted-id checks. Part or monolith, a
  non-null properties cell must hold a JSON object; other JSON fails the
  open with `property JSON must be an object` instead of loading as no
  properties. The loader decodes the cell with the WAL decoder,
  `props_from_json`, which already enforces this, and every writer since
  v0.4.0 writes an object. A null cell loads as no properties.
- **Duplicate ids on load.** Overlapping legacy parts can hold one id
  twice, and open accepts it. When a checkpoint load meets an id that
  already has a checkpoint-loaded version, the later row replaces that
  version entirely: labels, properties, and for a relationship its
  endpoints, type and adjacency. Each id is restored as exactly one live
  version, with one adjacency entry per endpoint, and duplicates are
  never rejected. A delete before the next checkpoint therefore leaves no
  copy behind for the heal to persist.
- **Restore contract.** A checkpoint loader ends in `Graph::finish_restore`,
  once, after its last row: it builds adjacency from the restored versions,
  builds the indexes, and clears the dirty ids the checkpoint holds.
  `Graph::rebuild_indexes` no longer builds adjacency: WAL replay after the
  load keeps adjacency current as a commit does, so replay ends in
  `rebuild_indexes` for the indexes alone, and an open builds adjacency
  once, not twice. A relationship the WAL tail deletes therefore keeps its
  stored hops after open, as the writer's own deletes keep theirs, where
  the second build used to drop them; no read at the opened commit
  timestamp or later expands them. `finish_restore` is `#[doc(hidden)]`,
  like `apply_replayed_redo`: a loader step, documented in the Rust spec
  for loader authors, not user API.
- **Fail closed on a bad copy source.** A copy-source failure in any
  absorbed part fails the whole checkpoint closed after one read of that
  shard: its previous shard is missing or unreadable, has a schema other
  than the shard schema (checked when it is opened, before the writer
  sees a batch), holds ids that are not strictly ascending, duplicates
  included, holds an id outside its part range, or lacks a clean row the
  unit copies. The published manifest and its parts stay, the WAL is not
  recycled, and the halving fallback does not retry it. Absorption widens
  the reach of a bad part: a write to a neighbour now reads it and fails
  the checkpoint too, and does not heal it.
- **Page envelope.** One file of tiny rows reaches ADR-0086's
  `MAX_PAGES` (65,536) before `MAX_WINDOW` rows: each `ROW_GROUP_CAP`
  (64-row) group writes a dictionary page and a data page per leaf
  column, 3 for nodes (`id`, the `labels` item, `properties`) and 5 for
  relationships (`id`, `start_id`, `end_id`, `type`, `properties`). A
  file therefore holds at most 64 * floor(65,536 / 6) = 699,008 node rows
  or 64 * floor(65,536 / 10) = 419,392 relationship rows, fewer when a
  large value spills into more data pages. The codec states that bound
  once, as `Kind::page_envelope_rows()`, and planning reads it from
  there. A unit of at most `MAX_WINDOW` ids whose live rows exceed it is
  written through the halving fallback at the cost of one extra encode
  per halving: one for nodes, two for relationships past 838,784 rows.
  Its halves then stay apart (Decision 2), and a seed wider than
  `MAX_WINDOW` ids is cut at the bound before it is written
  (Decision 3), so no unit pays more halvings than that, and only once.
  Bounding every unit's live rows up front would reintroduce the row
  budget the Alternatives reject.
- **Checkpoint I/O** per rewritten unit is bounded by target bytes of copy
  plus dirty encode. A one-row change can now copy up to `target` decoded
  bytes of neighbors, the budget pre-#195's whole-window re-encode spent,
  at copy cost. Files per checkpoint follow units and rewritten bytes,
  not dirty runs, so LABS-194's mid-pull checkpoints write a few node
  shards (one at `da2c13a3`) instead of thousands. No migration is needed.
- **Tests.** Existing tests that use `target_bytes = 1` still get one part
  per row: no two rows fit a target, so nothing merges. New persistence
  tests cover coalescing a fragmented range, the part-count bound, clean
  rows copied rather than re-encoded, the window-width bound, each row of
  a fitting unit measured once, an over-target seed measured once and
  copying clean rows across its files through one cursor, a neighbour
  over the row envelope kept as it is, an envelope retry that drops the
  copy cursor and recopies the rows behind it, a file with no rows
  refused before any halving, copy sources with a missing row, a
  duplicate id or another schema failing closed after one read, and
  overlapping legacy parts healing: a pair, a chain of three, a group
  over the target and wider than `MAX_WINDOW` ids, a healed window that
  copies the clean neighbours it absorbs, and a manifest listing one
  legacy part twice, which opens with one version per id and heals into
  disjoint parts that retire its file. Planning tests keep kept files
  apart at the real page envelope rows of each kind and at the target
  bytes, and cut a seed wider than `MAX_WINDOW` ids at the page envelope
  rows, by forging manifest rows or planning over ids the graph does not
  hold. A file halved for incompressible rows near the target is
  checkpointed twice more, and each write keeps the halves apart: it
  measures and reads one half and encodes only its dirty row.
- ADR-0022 is immutable; it gains a **Later references** note pointing
  here, and this ADR is the merge its Decision 4 left as follow-on.
- This ADR supersedes ADR-0180 Decision 3: clean runs are still copied,
  not re-encoded, but into the unit's byte-budgeted files instead of one
  part per run. ADR-0180's stream-and-split-on-compact-bytes rules
  (Decisions 1-2) stand. ADR-0180 is immutable; it gains a **Later
  references** note pointing here.

## Later references

ADR-0563 raises the `ROW_GROUP_CAP` this ADR's page-envelope arithmetic
carries (64-row groups → 1024-row groups), so a part's row bound rises
with it; the window, byte-budget and coalescing rules here are unchanged.
