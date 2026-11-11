# 0086. Bounded decoding of persisted WAL, bundles, and Parquet

- **Status**: Proposed
- **Date**: 2026-09-23
- **Amended**: 2026-09-24
- **Deciders**: CypherLite maintainers

## Context

ADR-0014 makes opening a store a Parquet snapshot load followed by WAL replay;
ADR-0072 applies the same VFS boundary to Native, Memory, and the single-host-file
Bundle of ADR-0074. ADR-0097 proposes exclusive ownership during recovery and
ADR-0098 proposes safe replacement of checkpoint objects. Neither ownership nor
atomic publication makes the bytes being recovered trustworthy.

Today a WAL frame's on-disk `u32` length directly sizes a `Vec`, and read errors
at the magic or next header can be mistaken for EOF. A checksum-valid `.cl`
superblock can claim arbitrary catalog length, page count, inode extents, or a
WAL-tail extension; recovery allocates from those claims and can reclaim pages
before proving they do not alias live data. Parquet's Snappy decoder allocates
from a page header's *uncompressed* size, not the compressed file size. The
Parquet 54.3.1 Arrow builder exposes footer/row-group/column metadata before
`build()`, but neither its batch-size setting nor its public page iterator
limits allocation **before** page decompression. The reviewed fallback is a
bounded pre-scan of actual column-chunk page headers in the supplied bytes (or
an equivalent reader hook that checks headers before decompression).

ADR-0022's 256 MiB default shard **target** is not a file-size maximum:
`CYPHERLITE_SHARD_TARGET_BYTES` is currently unbounded, a single row may exceed
the target, and older v1/v2 snapshots use monolithic Parquet files. A recovery
limit must account for valid larger shards and must also constrain the writer;
a blanket 256 MiB file cap would reject snapshots the engine itself can write.

## Decision

1. **WAL framing and tail.** A frame is an eight-byte CRC/length header plus
   **1..=64 MiB (67,108,864 bytes)** of JSON payload. Reject zero, greater
   than 64 MiB, or otherwise unrepresentable lengths before allocation on
   open, replay, and `records_after`; check the length against the
   remaining physical segment bytes using checked arithmetic. Refuse an
   oversized encoded commit *before* writing any frame bytes or advancing the
   published LSN. There is no 64 MiB cap on an entire WAL segment or on the
   number of committed frames.

   A complete magic followed by exact EOF on a frame boundary is normal, in
   either a closed or active segment. **Any** segment may end with a torn
   final frame: a short header (1–7 bytes), or a full
   in-range header whose final payload ends at physical EOF before its
   declared length. A complete final frame on the **highest, still-active**
   segment may also fail its CRC or JSON decode (a relaxed append). Replay the
   intact, CRC-checked preceding records; never interpret a trailing
   incomplete or corrupt frame as a commit, and trim the tolerated suffix
   before any later append. This relaxed tail tolerance aligns with ADR-0077's
   relaxed durability: a `snapshot` rotate does not barrier the closed
   segment, so a crash can leave the same torn suffix on a just-rotated closed
   segment. A missing, empty, or truncated magic on a previously existing
   segment, a complete frame that fails CRC/JSON anywhere other than the
   active segment's physical EOF, or an excessive length is corruption and
   fails open, with one narrow exception: after
    successfully validating every earlier segment (including the immediately
    preceding one), recovery may replace the highest segment's 1–7 bytes with
    a complete magic if its entire contents are an exact prefix of the magic.
    Such a segment cannot contain a frame. Replace it atomically through the
    VFS before opening for append; never repair an empty or sole segment, a
    non-prefix header, or any closed segment. A failed replacement fails open
    without changing the validated predecessor. Fresh segments are created
    with a complete magic. Use the observed file length and checked offsets to
   recognize a physical incomplete tail *before* reading; an actual VFS
   read/seek/length error, including an unexpected short read inside a range
   known to exist, always propagates. The same rules apply to `records_after`; a
   stale offset hint may be discarded and rescanned, but a real read error
   must not be retried as if it were a stale hint. Before any later append,
   remove or bypass the tolerated partial suffix without deleting the intact
   prefix, so future valid frames cannot be hidden behind it. For a `.cl`
   bundle, this logical EOF policy does **not** assert that an in-place append
   to a partially filled physical WAL page is crash-safe: ADR-0074's
   `BundleVfs::write_bytes` rewrites the full page, and a torn write can
   corrupt an earlier committed frame. A damaged committed frame fails CRC
   and is not a tolerated tail (ADR-0091).

2. **Bundle catalog and physical page ownership.** Obtain the real host-file
   length before allocating from a superblock: two complete 4096-byte
   superblock pages are required. Declared `file_pages` is at least two and
   no greater than `floor(host_len / 4096)`; neither a sparse file's apparent
   page count nor the declaration alone licenses a `Vec` or an unchecked
   per-page walk. Catalog byte length is at most **16 MiB (16,777,216
   bytes)** and at most the checked physical extent it names. The empty
   catalog has root and length zero together; a nonempty catalog starts at
   page >= 2, occupies `ceil(len / 4096)` pages, and ends within both the
   declared and physically complete pages. Bound decoded catalog entry counts
   by the remaining catalog bytes *before* reserve/iteration; use checked
   offsets, page additions, byte multiplication, and conversions throughout.

   Inode ids and names are unique; their nonzero-count extents and freelist
   ranges start at page >= 2, end within `file_pages` and the host file, and
   are pairwise disjoint from both superblocks, the active catalog, every
   other live inode extent, and each other (including freelist entries).
   Inode logical length must fit the checked sum of its mapped extent bytes.
   Validate catalog next-page/id values and the superblock's WAL inode,
   length, and proposed tail-page range against those same constraints before
   extending an inode or reclaiming orphans. A nonzero WAL hint must identify
   an existing WAL inode; its length cannot exceed validated capacity or
   reduce the catalog's committed length. Reclaim only after validating all
   ownership and tail ranges. Sweep/merge checked ranges rather than allocate
   a bitmap or iterate pages according to untrusted `file_pages`; no fixed
   whole-bundle size cap is imposed on a legitimately large `.cl` host file.
   Propagate failed reads of existing pages during writes instead of
   substituting zero pages; on recovery a real read error is not an invalid
   checksum or permission to use a different superblock and mutate state.
   Catalog writes must honor the same 16 MiB ceiling and checked page range
   before publishing a new superblock; a larger catalog fails explicitly
   without reclaiming pages still referenced by the old generation.

3. **Parquet recovery envelope, per shard.** Before `read_to_vec` or any
   equivalent whole-file allocation, require each node/relationship Parquet
   object (including a v1/v2 monolith) to be at most **1 GiB
   (1,073,741,824 bytes)** on disk. Before `try_new` reads the footer, require
   a checked footer-declared metadata length of at most **16 MiB**. Before
   `build()`, validate nonnegative file, row-group, and column counts/sizes,
   checked column offsets and chunk ranges within the file, no more than
   **1,000,000 rows per shard** (including the sum across row groups), and
   no more than **2 GiB (2,147,483,648 bytes)** summed uncompressed column
   bytes per shard. Check offsets *before* calling Parquet's `byte_range`,
   which asserts on negative offsets in version 54.3.1.

   Before any page decompression, parse actual page headers with a **64 KiB
   header-byte limit per page** and bounded Thrift nesting/container counts;
   permit at most **65,536 pages per shard**, with each compressed payload
   and uncompressed page at most **128 MiB (134,217,728 bytes)**. Require
   checked payload ranges inside their verified column chunks, nonnegative
   V1/V2 value counts (and V2 row counts) no greater than **1,000,000 per
   page**, and V2 repetition/definition lengths within the declared sizes.
   Checked sums of actual page
   uncompressed sizes across *all* columns and row groups also stay within
   **2 GiB**; metadata totals must not underreport the observed headers.
   Bound header parsing itself so malformed Thrift lengths cannot allocate
   before these checks. Apply the same policy to every accepted compression
   codec, including Snappy; a small compressed file is not an exemption.
   The stock Arrow builder's batch size or `PageReader::get_next_page` alone
   is insufficient because its decompression already allocates from the page
   header. Use a pre-scan or a page-reader hook with an equivalent
   before-allocation guarantee.

   Decode in batches of at most **1024 rows**, count actual rows across all
   batches before materializing them into the graph, and reject any actual
   count above **1,000,000** even if metadata claims less. Before parsing a
   row's property JSON or copying labels/strings into graph values, reject
   more than **384 MiB (402,653,184 bytes)** of combined decoded column value
   bytes for that row. Also reject once the *actual* sum of decoded row value
   bytes exceeds **2 GiB per shard**, even if dictionary encoding or metadata
   totals understated that work. A second row group must not reset the actual
   byte or row counters. Apply all checks identically in `load_nodes` and
   `load_rels`. Limits are per shard, not a limit on the
   total number of shards or graph size; graph objects and JSON expansion
   still incur additional memory beyond this persisted-input envelope.

4. **Writer symmetry and failure.** Keep 256 MiB as the default *target*,
   not the hard reader ceiling. Permit an explicit shard-target override up
   to **1 GiB**; reject larger or unrepresentable overrides rather than
   silently wrapping or clamping them. Even below that target, split a
   multi-row shard when necessary to meet the 1 GiB file, 1,000,000-row,
   128 MiB page, 384 MiB row, and 2 GiB decoded envelopes. ADR-0022's
   single-row over-target exception still applies *within* those hard ceilings: a
   300 MiB single-row shard is not rejected merely for exceeding the
   default target, but an unsplittable row/page exceeding the envelope
   fails checkpoint explicitly. Configure/check writer page output too;
   the Parquet library's nominal page-size target is not a hard guarantee.
   Never publish a new manifest or recycle the WAL after an out-of-envelope
   checkpoint failure; the old checkpoint and committed WAL remain available.
   Persisted legacy WAL frames, bundles, or snapshots that exceed these
   limits or fail validation return a storage/corruption error on open or
   replay, with the offending object and bound identified. Do not silently
   truncate, discard committed records, replace with an empty graph, or
   auto-rewrite legacy bytes. A legitimately larger legacy snapshot needs
   an explicit offline repartition/migration with a reader that can handle
   its size before it can use this envelope.

5. **Manifest, pull map, paths and numeric sentinels.** Cap each
   `manifest.json` and pull-identity JSON object (legacy
   `pull-identity.json` or a manifest-selected object under ADR-0092) at
   **16 MiB (16,777,216 bytes)** independently. Check VFS metadata *and*
   the opened handle's length before a `Vec` allocation or `read_to_vec`,
   then check exact read/unchanged length; apply this at codec resolution,
   snapshot load and checkpoint/identity accounting, not only at one call
   site. Writers must reject an over-limit manifest or map before publishing
   it, leaving the old checkpoint and WAL recoverable. Preserve legacy
   missing pull-map-as-empty behavior only for manifests lacking a selector;
   a selected map that is absent or exceeds the bound is an error. This is a
   per-object cap, not an aggregate cap on the number of shards or pull
   entries; JSON still must be parsed fallibly within its checked envelope.

   Before opening, retiring or deleting any v3 part, validate its manifest
   path against its kind and half-open `id_lo`/`id_hi`: exactly
   `nodes/{lo}-{hi}.parquet` or `relationships/{lo}-{hi}.parquet` for legacy
   parts, or the ADR-0089 version with `-{32 lowercase hex}` immediately
   before `.parquet`. Require canonical decimal nonnegative bounds with
   `lo < hi`, matching the declared range, one relative path separator, and
   no other components or separators. Refuse absolute paths, `..`, empty
   components, backslashes, drive prefixes, reserved names such as `.cypherlite.lock`
   and `wal/.cypherlite.lock`, and symlink/reparse-point aliases at either the shard
   directory or leaf on Native. Do not turn a malformed path into a cleanup
   candidate or recycle WAL on failure; a syntactically valid missing part
   is corruption, not an empty shard. Validate before *any* VFS open/read or
   deletion, including mixed legacy/new v3 manifests. Existing correctly
   named v3 shards and v1/v2 monoliths remain readable (ADR-0089).

   Treat persisted `commit_ts == u64::MAX` (WAL or manifest) and node or
   relationship ID `== i64::MAX` (Parquet, including relationship endpoint
   IDs) as unsupported sentinels: return typed `Error::Storage` rather than
   allowing a `+ 1` in graph restoration/next-write setup to panic or wrap.
    One-below values are valid if their next counters are representable;
    validate manifest next-ID counters and future increments before mutation,
    and return a typed error at exhaustion instead of allocating/writing a
    sentinel. The public low-level `Graph::create_node` and
    `Graph::create_rel` APIs return `Result<NodeId>` and `Result<RelId>`;
    allocate with a checked atomic reservation so concurrent callers cannot
    claim the sentinel or wrap the counter. Both these methods and the
    private `ExecGraph` write view report `Error::Execution` (`node ID
    exhausted` or `relationship ID exhausted`) before graph mutation or redo
    emission. Restoring an invalid ID (including relationship endpoints) or
    incrementing a restored ID beyond the supported range reports
    `Error::Storage` before insertion. Apply the same check to writer output
    and WAL append so newly persisted data can be reopened. A
    malformed/oversized legacy object is left unchanged for explicit
    recovery, not silently normalized. The public low-level
    `Graph::begin_write() -> Result<()>` and
    `Graph::begin_write_at(write_ts) -> Result<()>` reject a next or explicit
    write timestamp of `u64::MAX` with `Error::Execution` before changing
    write state, pending redo, or statistics. `begin_write` must also avoid
    overflow when computing the next timestamp. A valid committed
    `u64::MAX - 1` can be read/recovered, but cannot begin another implicit
    write. `Graph::apply_redo(ops, commit_ts)` rejects a WAL commit timestamp
    of `u64::MAX` with `Error::Storage` before installing any operation or
    changing write state. Valid timestamps below the sentinel retain their
    existing behavior. In snapshot byte accounting, propagate VFS metadata
    errors for present objects rather than treating them as missing, and
    check the opened pull-identity object's length against the same 16 MiB
    bound before accounting for it.

6. **Streaming recovery, stable explicit replay API.** Inspect each valid
   WAL frame with the same CRC, length, EOF/torn-active-tail and read-error
   checks from (1), but derive `Wal::open`'s LSN, commit count and last
   timestamp by folding over records rather than collecting segment-sized
   `Vec<WalRecord>`s. Apply records above the manifest's checkpoint LSN to
   the graph one at a time during `open_store`/`load_with_wal`; never retain
   the entire valid log in memory just because every frame is in bounds.
   Preserve ordering and commit/identity operations from ADR-0080, CRC
   failures that are not a tolerated tail, `records_after`'s limit/hint
   semantics, and the any-segment torn-tail / active bad-EOF policy. Keep the
   `Wal::replay(from_lsn) -> Result<Vec<WalRecord>>` API for callers that
   explicitly request materialized records; only internal startup/recovery
   changes to streaming. A failure partway through recovery fails the open
   without publishing a partially replayed graph or recycling its WAL.

## Alternatives considered

- **Trust on-disk lengths and page headers and allocate on demand.** Rejected:
  forged WAL/catalog lengths and high-ratio Snappy pages can reserve far more
  memory than their physical files and fail before a checksum or decode error.
- **Treat every WAL read failure as clean EOF or a torn tail.** Rejected:
  actual I/O faults and corruption before the tail would silently hide
  previously committed records; only a delimited incomplete tail on any
  segment, or the active segment's bad-EOF final frame, is recoverable.
- **Limit only the compressed Parquet file or Arrow batch size.** Rejected:
  decompression allocates from page headers before batching; a tiny file can
  declare a huge uncompressed page and metadata may understate page sizes.
- **Cap every shard at the 256 MiB default target, or keep an unlimited
  override.** Rejected: ADR-0022 permits single-row over-target shards and
  configurable targets, while an unlimited target makes recovery bounds
  ineffective. A larger hard ceiling with symmetric write checks retains
  larger supported shards without promising arbitrary-size recovery.
- **Cap the entire `.cl` file to bound a forged `file_pages` value.** Rejected:
  a valid bundle accumulates many WAL, catalog, and snapshot pages; validate
  referenced ranges against physical pages and avoid page-count-sized
  allocations instead.
- **Repair malformed old files automatically during open.** Rejected: a
  guessed repair can discard committed WAL bytes or alias live bundle pages;
  an explicit error preserves the original bytes for offline recovery.
- **Rely on the 1 GiB shard limit to bound the manifest or pull map.** Rejected:
  both JSON files are read independently, and sparse metadata lengths can
  allocate huge buffers before any Parquet preflight is reached.
- **Trust VFS path normalization or accept arbitrary manifest part paths.**
  Rejected: a checksum-valid v3 manifest could target a Native lock inode or
  external file and make open/retirement corrupt an unrelated identity.
- **Collect every valid WAL record in startup because each frame is capped.**
  Rejected: total segment/record count is deliberately uncapped, so a valid
  log can consume unbounded recovery memory even with a 64 MiB frame cap.
- **Remove or change `Wal::replay` to enforce internal streaming.** Rejected:
  it is a public API whose explicit materialization contract can coexist
  with a bounded internal recovery cursor.
- **Keep infallible low-level `Graph::create_node` and unchecked atomic
  `fetch_add` allocation.** Rejected: direct callers can reach exhaustion
  after a valid one-below-maximum snapshot, and concurrent callers can wrap
  or reserve the unsupported sentinel before the write view checks it.
- **Panic or saturate the public graph allocator at the sentinel.** Rejected:
  neither exposes a typed, recoverable failure; saturation can duplicate an
  ID and contaminate graph state or redo.
- **Keep infallible `Graph::begin_write`/`begin_write_at` and rely on higher
  layers or persisted manifest validation.** Rejected: direct low-level
  callers can reach the timestamp sentinel from a valid one-below-maximum
  graph, or supply it explicitly, before any higher-layer guard runs.
- **Treat a rejected local write timestamp as `Error::Storage`, or a WAL redo
  timestamp as `Error::Execution`.** Rejected: a locally requested write is
  runtime exhaustion, whereas an invalid timestamp in replay is malformed
  persisted input; distinguishing them preserves the typed error boundary.
- **Silently skip failed snapshot metadata lookups as absent objects.**
  Rejected: a real VFS error or an oversized pull-map handle must not produce
  a deceptively small snapshot-size report.

## Consequences

- WAL replay and `records_after` share one finite frame/tail contract. Recovery may
  return the intact committed prefix before an incomplete tail on any segment
  or a corrupt final active frame, while interior corruption and genuine I/O
  faults fail explicitly.
- `.cl` recovery has a bounded catalog allocation and checked page ownership
  before WAL-tail extension or orphan reclamation, without an arbitrary cap
  on total host-file size. Native, Bundle, and Memory persistence roundtrips
  must demonstrate that valid pages and records remain readable.
- Node and relationship Parquet recovery must preflight metadata *and actual
  page headers* before decompression, then count actual decoded rows and bytes.
  The default target remains 256 MiB; valid shards can be larger, up to the
  documented hard ceilings. Writer limits and legacy-file failures are
  observable and need matching Rust API documentation and regression tests.
- Manifest and pull-map reads and writes have independent 16 MiB ceilings;
  strict v3 path validation protects Native lock identities before opens,
  retirement or orphan cleanup. Numeric sentinel values return typed errors
  instead of a panic; public low-level graph creation and write-begin methods
  are fallible and their callers must handle exhaustion. WAL redo reports a
  storage error for the timestamp sentinel before applying operations; snapshot
  byte accounting propagates metadata faults and bounds opened identity maps.
  Larger/invalid legacy inputs
  remain intact on failure.
- WAL open and graph reconstruction must retain only a bounded number of
  decoded records at a time even for an arbitrarily long *valid* log;
  callers of the unchanged public `Wal::replay` can still request a `Vec`.
- Future changes to per-format limits or recovery compatibility require a
  new decision and an explicit migration story rather than silently raising
  allocations or accepting a checkpoint that cannot be reopened.

## Later references

ADR-0114 narrows §1: a complete final frame on the active segment is a
tolerated relaxed append only when its CRC fails. A frame whose CRC matches
but whose payload does not decode is corruption in every position. ADR-0114
also validates the snapshot's format marker before WAL recovery starts.

ADR-0120 adds the WAL segment magic `MDBWAL02` next to `MDBWAL01`. The rules
in §1 apply to both, and the short-header repair writes `MDBWAL01`. ADR-0120
also reuses this ADR's 16 MiB object envelope as the ontology document size
cap, checked before the commit is written.

ADR-0284 keeps a shard page header inside the 64 KiB limit in §3. The writer
stores no page statistics, so a large property value stays in the page payload
instead of being copied into the header as a min/max.
