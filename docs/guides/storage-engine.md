# Storage Engine

The matcher runs on an **in-memory** labeled property graph (interned records,
typed adjacency chunks, MVCC). Disk is for crash recovery, not for scanning.

There is **one** write path ([ADR-0014](https://github.com/relatedb/relate/blob/main/docs/adr/0014-single-wal-write-path.md)):

1. **Open** — load one snapshot codec from the directory and validate its
   format version (Parquet manifest `format_version` 1–4, where 4 marks a
   manifest selecting the store's immutable ontology object; JSON
   `value_encoding` absent, 2 or 3, where 3 is encoding 2 plus the inline
   ontology document, `read_only` false) before WAL recovery
   ([ADR-0114](https://github.com/relatedb/relate/blob/main/docs/adr/0114-wal-recovery-rejects-undecodable-records.md),
   [ADR-0120](https://github.com/relatedb/relate/blob/main/docs/adr/0120-ontology-document-persistence.md)),
   then replay WAL records with `lsn > checkpoint_lsn`. A supported-older
   store is migrated to the current format first, before any query is
   served — see [Store versions and migrate-on-open](#store-versions-and-migrate-on-open).
2. **Commit** — append physical redo to the WAL and fsync, then `apply_redo`.
   The live graph is never the source of a commit.
3. **Checkpoint** — pin an LSN, dump codec-owned store files, recycle closed
   WAL segments.

Parquet (or `graph.json`) is a snapshot of **already-committed** state. Queries
do not read those files.

GraphStore (standalone JSON `.db` files, Lance, in-memory `MemoryStore`,
`--legacy`) is withdrawn
([ADR-0052](https://github.com/relatedb/relate/blob/main/docs/adr/0052-graphstore-withdrawn.md)).

## Snapshot codecs

**One codec per directory** ([ADR-0050](https://github.com/relatedb/relate/blob/main/docs/adr/0050-snapshot-codecs.md)).
Mixing `graph.json` with Parquet shards is an error. A mismatched
`--snapshot-format` on an existing directory is an error.

| Codec | How to choose (empty URI) | On-disk store files | Checkpoint |
|-------|---------------------------|---------------------|------------|
| **parquet** (default; requires the `parquet` feature) | omit flag, `--snapshot-format parquet`, or `RELATE_SNAPSHOT_FORMAT=parquet` | `manifest.json`, `nodes/{lo}-{hi}.parquet`, `relationships/…` | Rewrite dirty id windows ([ADR-0022](https://github.com/relatedb/relate/blob/main/docs/adr/0022-id-window-checkpoint-shards.md)) |
| **json** (default without the `parquet` feature) | `--snapshot-format json` or `RELATE_SNAPSHOT_FORMAT=json` | `manifest.json`, `graph.json` | Full rewrite of GraphData |
| **markdown** ([ADR-0406](https://github.com/relatedb/relate/blob/main/docs/adr/0406-markdown-vault-snapshot-codec.md)) | `--snapshot-format markdown` or `RELATE_SNAPSHOT_FORMAT=markdown` — an empty directory becomes the vault root | One `Label/name.md` per node (frontmatter properties, body text, `[[wikilinks]]`), regenerated `index.md`/`log.md` views, and a `.cypherlite/` engine sidecar (manifest, WAL, journal) | Rewrite changed guarded files, regenerate views wholesale; hand edits made while the database was closed are picked up at the next open — see the [Obsidian vault guide](obsidian-vault.md) |
| **`:memory:`** | URI `:memory:` | In-process `MemoryVfs` + WAL | Process-ephemeral; no OS files. Automatic checkpoints only recycle the WAL; an explicit `checkpoint()` writes an in-process snapshot |

The durable forms — Parquet for machine efficiency, JSON for readable
snapshots, the markdown vault for human-authorable knowledge bases, and the
single-file `.rl` bundle — are compared in the
[storage formats reference](../reference/storage-formats.md).

Every codec's checkpoint also rewrites the store's **schema text mirror**
under `.cypherlite/schema/` (ADR-0481) — the graph type, the ontology
document, and the index/constraint catalog as canonical text files. See the
[schema folder guide](schema-as-files.md).

WAL layout (`wal/`, segment magic `MDBWAL01`, or `MDBWAL02` for segments
created while the store holds an ontology —
[ADR-0120](https://github.com/relatedb/relate/blob/main/docs/adr/0120-ontology-document-persistence.md))
does not change with the codec.
The markdown store records its codec in the `.cypherlite/manifest.json`
sidecar, not a root `manifest.json`; directory codecs record
`"codec": "parquet" | "json"` there (absent codec means
parquet). Existing files win over the flag.

The markdown vault codec ([ADR-0406](https://github.com/relatedb/relate/blob/main/docs/adr/0406-markdown-vault-snapshot-codec.md))
makes a folder of notes the store itself; which node properties render as
each note's body is configured per label
([ADR-0460](https://github.com/relatedb/relate/blob/main/docs/adr/0460-markdown-body-properties.md)):
`--markdown-body-properties summary,notes;Book=summary,review` renders the
list's first entry as the bare body and each subsequent one as a
`## <property>` section — Obsidian-readable, hand-editable, and recorded in
the vault's `.cypherlite/manifest.json` so later opens need no flag. The
unconfigured form is the single `body` property. See the
[Obsidian vault guide](obsidian-vault.md).

The Parquet codec is a default-on Cargo feature, `parquet`, of `relate`
and `relate-storage`
([ADR-0105](https://github.com/relatedb/relate/blob/main/docs/adr/0105-optional-parquet-codec.md)).
A build without it creates JSON stores. Opening a Parquet store, or asking for
Parquet, in such a build fails at open with this storage error, with no
fallback to JSON:

```text
parquet snapshot codec is not compiled into this build (enable the "parquet" feature)
```

Create stores with the JSON codec if builds with and without
the feature must share them. wasm32 is JSON-only either way. See
[Cargo features](../api/rust/spec.md#cargo-features).

```bash
./relate -d ./mydata
./relate -d ./mydata-json --snapshot-format json
```

```rust
use relate::{Database, DurabilityMode, OpenOptions, SnapshotCodecKind};

let parquet = Database::open("./mydata")?;
let json = Database::open_with(
    "./mydata-json",
    OpenOptions {
        wal_dir: None,
        checkpoint_interval: None,
        durability_mode: DurabilityMode::Full,
        logical_graph_size_limit: None,
        snapshot_codec: Some(SnapshotCodecKind::Json),
    },
)?;
```

C and Python: set `RELATE_SNAPSHOT_FORMAT=json` before opening an
**empty** directory to select GraphData JSON. Import an old Relate `.db`
file by copying it to `<dir>/graph.json` and opening with `--snapshot-format json`.

## Store versions and migrate-on-open

Every durable subsystem carries a version marker, and one registry in
`relate-storage` (`src/version.rs`) names, per subsystem, what this
binary writes (`CURRENT_VERSION`) and the oldest version it can read
([ADR-0127](https://github.com/relatedb/relate/blob/main/docs/adr/0127-store-version-compatibility.md)):

| Subsystem | Version marker | Writes | Readable |
| --- | --- | --- | --- |
| Parquet snapshot | manifest `format_version` | 3 (4 while the store holds an ontology) | 1–4 |
| JSON snapshot | `graph.json` `value_encoding` | 2 (3 while the store holds an ontology) | absent (1), 2 or 3 |
| WAL | segment magic | `MDBWAL01` (`MDBWAL02` while the store holds an ontology) | `MDBWAL01`, `MDBWAL02` |
| `.rl` bundle container | superblock magic + `VERSION` | `MDBCLB01` / 1 | `MDBCLB01` / 1 |

The ontology-dependent markers follow the content
([ADR-0120](https://github.com/relatedb/relate/blob/main/docs/adr/0120-ontology-document-persistence.md)
§8): a store that never sets an ontology stays byte-for-byte in the
v3/encoding-2/`MDBWAL01` format, and after the ontology is dropped, a
checkpoint publishes and the `MDBWAL02` segments are recycled, the store is
back at it. A v4 manifest is a v3 manifest plus `ontology_path`; encoding 3
is encoding 2 plus the inline `ontology` field. Older binaries fail closed
on an ontology-carrying store — at the `MDBWAL02` segment, the v4 manifest
or `value_encoding: 3` — before modifying any data.

The contract is **migrate-on-open: hands-off, forward-only**. There is one
legacy reader per readable version plus the current writer, and detection
is the marker the codec loader already decoded — no separate pre-read of
`manifest.json`, `graph.json` or WAL headers. Opening a store whose
markers are readable but not current upgrades it in place, with no user
action, before the first query is served, in one open flow: load with the
legacy reader for the reported marker, `Wal::open`, WAL replay, then — on
a durable VFS only — a checkpoint at the current format and a WAL rotation
to a fresh current-magic segment, all on the handles already open (no
second load). A second open is at current format and migration is a no-op.
There is no downgrade path: once migrated, older binaries reject the store
at its version markers.

- All version markers are validated **before any byte mutates** (the
  ADR-0114 ordering), so a store written by a **newer** Relate still
  hard-fails at open with the unsupported version in the error.
- A corrupt or partial old store fails loudly with the detected version
  named in the error; it is corruption, not a migration input.
- A crash mid-migration leaves the old store intact (manifest-last
  tmp/rename publication); the next open simply re-migrates.
- `:memory:` never migrates — no durable state.
- The `.rl` container format is frozen: a bundle migrates the payload
  files inside it, never the container layout.
- The WAL has two readable segment magics. `MDBWAL01` is the default write
  magic; `MDBWAL02` heads segments created while the store holds an
  ontology (ADR-0120, implemented), so older binaries fail closed before
  they could misread an ontology record. Both magics are current content,
  not format ages: the WAL scanner and the native-ownership admission
  accept either, and migrate-on-open never rewrites WAL segments — a fresh
  segment simply takes the magic matching the store's current ontology
  state. A future `RedoOp` shape change bumps the magic again and ships a
  legacy decoder for the previous one instead of failing loudly with no
  path, with legacy-magic detection inside `Wal::open`, which forces a
  fresh current-magic segment even when the WAL tail is empty.

Migration is internal: the `MigrationReport` type is `pub(crate)` and no
user-facing API or log line exists yet.

## Persistence model

Redo is **physical graph ops** (create/delete node and rel, set/remove
property, add/remove label), not Cypher text.

WAL and snapshot files go through a synchronous
[virtual filesystem](https://github.com/relatedb/relate/blob/main/docs/adr/0072-virtual-filesystem.md)
(`NativeVfs` for a directory path/`file://`, `BundleVfs` when the path
ends with `.rl` — [ADR-0074](https://github.com/relatedb/relate/blob/main/docs/adr/0074-single-file-bundle-vfs.md) —
`MemoryVfs` for `:memory:`). The virtual tree is unchanged; a `.rl` file
is that tree packed into one host file. `.cl` is still accepted as the
legacy bundle extension so v0.16-and-earlier bundles keep opening
([ADR-0517 §5](https://github.com/relatedb/relate/blob/main/docs/adr/0517-rename-to-relate.md)).

## Read-only open and follow

`--read-only` / `OpenMode::ReadOnly` loads the snapshot and WAL once and
then does not write: no append, checkpoint, migrate-on-open, orphan sweep,
or torn-tail trim (ADR-0136). A native directory can stay open beside the
writable process. Several read-only opens can share it. A second writable
open still fails with `Error::Locked`. A `.rl` bundle stays exclusive, so a
read-only bundle opens only while no writer holds the file. A missing path
is an error and is not created. `:memory:` with `--read-only` rejects writes
and has no files. Reopen a frozen reader to see later commits.

`--read-only=live` (the former `--follow`, ADR-0137/ADR-0480) is the
live freshness of the one access flag. It is only for a native
directory and is refused for `.rl` and `:memory:`. A thread applies
complete WAL frames that appear after open.
Running statements keep the snapshot they pinned. If a checkpoint recycles
a segment the follower has not applied, or the active segment shrinks, the
follower reloads the current snapshot and the WAL that remains. It does not
pin WAL recycle and it does not trim.

`Database::clone` / `relate clone SOURCE DEST` (ADR-0156, ADR-0282) is that frozen
read-only load, then a checkpoint published only into a new store. Each side
is a native directory, or a single-file bundle when its path ends in `.rl`.
The
source is not written and the clone does not follow later commits. The
destination is a new store at the loaded LSN, with an empty WAL (under
`<dest>/wal` for a directory destination).
`:memory:` is refused. A failed clone removes the destination file or
directory it created.

On a filesystem directory URI:

```
<dir>/manifest.json
<dir>/nodes/{lo}-{hi}.parquet    # parquet codec
<dir>/relationships/{lo}-{hi}.parquet
<dir>/graph.json                 # json codec instead of shards
<dir>/.cypherlite/schema/        # schema text mirror (ADR-0481)
<dir>/wal/NNNNNNNN.log
```

```rust
use relate::Database;
let db = Database::open("./graph.rl")?; // one host file (ADR-0074)
```

- `full` durability (the default) barriers the WAL before acknowledging a
  group. `everysecond` acknowledges after apply and barriers dirty WAL records
  approximately once per second; a failure can lose the latest approximately
  one second of acknowledged writes, and slow scheduling/storage can extend
  that window. `snapshot` acknowledges without a WAL barrier; only a completed
  checkpoint is durable, so a crash can lose all writes since that checkpoint.
- Select a persistent database policy with
  `--durability-mode full|everysecond|snapshot` or
  `OpenOptions::durability_mode`. This setting is not stored in the database.
- Crash recovery: load snapshot, replay the log. Uncheckpointed committed
  statements survive; an overlay that never reached WAL does not. A
  checksum-valid WAL record that does not decode fails open
  ([ADR-0114](https://github.com/relatedb/relate/blob/main/docs/adr/0114-wal-recovery-rejects-undecodable-records.md)).
- `:memory:` uses the same types in-process. Nothing is written to disk.

Engine object-store URIs (`s3://`) are withdrawn for this product
([ADR-0044](https://github.com/relatedb/relate/blob/main/docs/adr/0044-withdraw-engine-s3.md)). Durability is a **local
directory** or a **`.rl` bundle file**.

Checkpoint is rotate, dump, recycle — not stop-the-world truncate. The open
WAL segment closes at **64 MiB**, **10 000 commits**, or a **timer** (default
300 s, `RELATE_CHECKPOINT_INTERVAL_SECS`; `0` disables the clock). Writers
continue on the next segment. Persistent URIs also checkpoint on close.
For `:memory:`, the rotation and close checkpoints skip the dump and only
recycle the WAL (nothing can recover from that snapshot); an explicit
`checkpoint()` still writes one.
In `snapshot` mode, use `--checkpoint-interval-secs N` to set the normal
time-based loss window. `0` disables only timed checkpoints; size/count,
explicit, and clean-close checkpoints still run.

## Autocommit vs explicit transactions

**Default is auto-commit:** one Cypher statement is one WAL commit. Isolation
for readers is a snapshot at statement (or transaction) start.

**Explicit transactions** (`.begin` / `.commit` / `.rollback`, `BEGIN` /
`COMMIT` / `ROLLBACK` on a connection):

- Nested begin is an error.
- Statements in the transaction share one overlay and one snapshot pin.
- `COMMIT` appends the combined redo once. Empty commit is a no-op.
- Connection drop or `.quit` with an open transaction **rolls back**.
- After a statement error in a transaction, further statements fail until
  rollback (“poisoned” transaction).
- `CALL { … } IN TRANSACTIONS` runs batches as their own durable units inside
  the enclosing statement; it is not a general savepoint mechanism.

openCypher TCK scenarios stay auto-commit.

## Live graph

- Labels, relationship types, and property keys are **interned**.
- Adjacency is **typed chunks**: per node, `type_id → Vec<RelId>`. Deletes do not punch
  holes; visibility is `rel_at` / `node_at` at `snapshot_ts`.
- Versions carry `xmin` / `xmax`. Incremental GC drops versions no pin still
  needs.
- Snapshot pins are a lock-free slot table. The query path does not take a
  database `RwLock`. Readers and one `apply_redo` can run together; WAL
  append still serializes commit.

## Memory cap

Optional **logical** limit on the live graph and data-dependent indexes
([ADR-0042](https://github.com/relatedb/relate/blob/main/docs/adr/0042-graph-memory-limit.md)), not process RSS.
Growing writes that would exceed it fail with `Error::Capacity` at WAL
append. `DELETE` and shrinking `SET` stay allowed. Open and replay always
apply.

`RELATE_LOGICAL_GRAPH_SIZE_LIMIT` / `--logical-graph-size-limit` / `OpenOptions.logical_graph_size_limit`
accept bytes or `mb` / `mib` / `gb` / `gib`.

There is no buffer pool or spill-to-disk. **The working set must fit in RAM.**

## Further reading

WAL, MVCC, checkpoint, planner, and indexes in depth:
[`../engine-architecture.md`](../engine-architecture.md).
