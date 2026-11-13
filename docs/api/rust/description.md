# Rust crate — description

The `relate` crate is the native embedding API: open a database, obtain a
connection, run OpenCypher, inspect tabular results. Every **auto-commit**
write appends redo to a WAL and then applies it to the in-memory graph; open
materializes the snapshot (Parquet shards or `graph.json`) plus WAL replay (ADR-0014). `:memory:` is the same path
with in-process store and WAL. `Database::open_vfs` runs that path on a caller-supplied VFS (the wasm OPFS pool, ADR-0145); `opfs://` is not a `Database::open` URI. A path ending in `.rl` is one host file
(ADR-0074). Persistent URIs use the configured `OpenOptions.durability_mode`
and checkpoint with
the snapshot codec (Parquet by default when the `parquet` Cargo feature is
compiled in, JSON GraphData otherwise; ADR-0050, ADR-0105). Concurrent connections share one database: reads are MVCC snapshots
(ADR-0013, ADR-0018); writes are optimistic and serialize at WAL append
(ADR-0015, ADR-0016 overlay). `begin` / `commit` / `rollback` group statements
into one WAL commit (ADR-0028); other connections do not see uncommitted
writes. Every commit is a resumable position: an auto-commit write's
`QueryResult.bookmark` and an explicit `commit()`'s return carry the
store's commit sequence in bookmark form, `relate:<n>` (ADR-0310's
form, ADR-0502 §7) — monotonically increasing per store, `None` when
nothing committed. A connection may carry an optional authenticated **principal**
(`Connection::set_principal` / `principal`, ADR-0273): a display-only
identity a surface sets after verifying it (the Bolt server sets it after a
validated HELLO); `dbms.showCurrentUser()` reports it as `username`, and it
confers no authorization. Connections whose surface never sets a principal
see the documented placeholder `'relate'`. A network transport that
reports planning failures and execution failures at different points (Bolt
answers the first at RUN and the second at PULL) runs statements through
`Connection::query_staged`, which tells the two phases apart, and passes a
`Heartbeat` to keep its client connection alive while a long statement or
COMMIT runs on the query worker (ADR-0312, ADR-0319). That transport also
asserts its explicitly stacked connection threads
(`Connection::assume_worker_stack_on_caller_thread`, ADR-0504), so
bounded reads run on the connection's own thread without the worker
handoff, and only the statements that can outlive a keep-alive interval
— every write among them — escalate (ADR-0259's guarantees hold on both
paths).
`Full` (the default) shares one WAL barrier among independent waiters
and acknowledges after it. `EverySecond` acknowledges after apply and can lose
the latest approximately one second of writes; `Snapshot` can lose everything
since the latest completed checkpoint (ADR-0077). Independent eligible waiters share one WAL `sync_all`; SUCCESS follows
that barrier and ordered graph application (ADR-0019). Each proposed record is
size-counted by the same bounded JSON encoder used for append, and the entire
eligible group is checked for ordered graph applicability before any frame is
written. This adds serialization CPU, but does not buffer an encoded group.
Physical WAL uncertainty fences new read admission, replication, and
checkpoints on the current handle until reopen (ADR-0094). A read already
admitted with a pinned safe graph timestamp and schema snapshot can complete
after a later fence, without waiting for the WAL barrier. SHOW INDEXES,
SHOW CONSTRAINTS, SHOW GRAPH TYPE, SHOW CURRENT GRAPH TYPE and SHOW DATABASES
use the schema and theater captured with that pin; the database rows in that
metadata are the settings of the three fixed namespaces of ADR-0116, the
only database state the catalog holds. Explicit transactions keep
their begin-time entity pin and check health and capture committed schema and
theater at each statement admission; their own DDL metadata stays private in
the redo overlay until commit and disappears on rollback. The executor exposes
`execute_read_at` and `ExecGraph::read_at_with_schema` for callers that already
own a `Graph::pin_now()` guard and its admitted `SchemaSnapshot`. The matching
`ExecGraph::write_with_schema` applies a private `WriteScratch` overlay to
statement-admitted schema while retaining the scratch's entity timestamp;
`RedoOp::CatalogDerivedConstraint` carries standalone enforced-label SHOW
metadata through the same durable commit and replay as its constraint;
checkpoint images preserve validated typed metadata with the named catalog
(ADR-0095). Older snapshots or historical WAL lacking exact enforced-label
metadata retain their legacy defaults through narrowly validated markers in
typed checkpoint images, alongside all newly committed SHOW metadata. New incomplete
constraints fail pre-append or checkpoint without modifying durable state.
Malformed typed images fail recovery rather than relaxing enforcement. Storage
codecs use `Graph::checkpoint_theater_for_write` and `restore_checkpoint_theater`
to round-trip known metadata and exact legacy-unknown definitions. Legacy GraphData import uses
`restore_legacy_enforced_constraints` when an older JSON constraint supplies
an enforced label. See the
[compiled pinned-read example](examples/pinned_read.md). Direct storage WAL
handles also fence further writes and recycling after a physical write, sync
or rotation error.
Bounded preflight proofs let group commit validate and encode each candidate
once; the proof owns the encoded payload that its append writes as the frame.
A checkpoint already in progress finishes
before the next frame is written; checkpoints otherwise dump pinned snapshots
and recycle closed WAL segments (ADR-0020).

The Parquet codec is an on-by-default Cargo feature, `parquet`, on both
`relate` and `relate-storage` (ADR-0105). Depending on `relate`
with `default-features = false` drops Arrow and Parquet from the build. Such a
build creates JSON stores and refuses Parquet stores, or an explicit Parquet
request, at open with a typed `Error::Storage` that names the missing feature.
It never falls back to JSON. A manifest without a `codec` field still means
Parquet. See [Cargo features](spec.md#cargo-features).

`:memory:` has no restart to recover from, so its automatic checkpoints (WAL
rotation and close) write no snapshot; they only recycle closed WAL and GC old versions (ADR-0105). An explicit
`Database::checkpoint()` writes a snapshot on every store.

Entity IDs use `i64::MAX` as an unsupported persisted sentinel (ADR-0086).
The executor's private write view returns a typed execution error when a node
or relationship allocator reaches that boundary, before producing WAL redo.
The low-level `Graph::create_node` and `Graph::create_rel` calls reserve IDs
atomically and return the same typed error before mutating on exhaustion;
checkpoint restoration rejects unsupported IDs with `Error::Storage`.
`ExecGraph::create_node` is fallible for the same reason;
see the [runnable ID exhaustion example](examples/id_exhaustion.md).
The low-level graph also reserves `u64::MAX` as an unsupported write timestamp:
`begin_write` and `begin_write_at` return `Error::Execution` before modifying
write state or redo at that boundary, while `apply_redo` rejects persisted WAL
timestamps at the same boundary with `Error::Storage`. A valid commit at
`u64::MAX - 1` can be read but cannot start a further implicit graph write;
the [compiled example](examples/id_exhaustion.md) exercises these paths.

Independent writable opens, including another process or a root sharing an
overridden WAL directory, take a shared lock on `.cypherlite.lock` and an
exclusive `.cypherlite.writer` lease before recovery (ADR-0097, ADR-0136).
A read-only open takes only the shared identity locks, so it can share a
native directory with the writer. Native uses a stable `.cypherlite.lock` in
the database root and another in its WAL directory, including an external
override; `.rl` locks its single host file without a sidecar, and a
read-only bundle cannot share that file with a writer. Contention promptly
returns the typed `Error::Locked`, whose display starts `storage locked:`. Retaining a connection
keeps the owner alive after the `Database` handle drops; final persistence is
protected before release, and process exit releases the OS lock. `:memory:`
has no cross-open ownership. An incomplete reset fails closed so a fresh root
cannot replay an old WAL. Newly created Native paths are private on Unix
(directories `0700`, files `0600`); a newly created `.rl` bundle is one `0600`
host file. Existing permissions are preserved; Windows inherits ACLs. See the
[runnable exclusive-open example](examples/storage_lock.md).
Persistent Native roots cannot contain nested Native databases or `.rl`
bundles. Reset detects pre-existing nested identities in both the root and any
disjoint external WAL tree and refuses to delete them, even if an independent
owner is still live; move a legacy nested database elsewhere before resetting
its parent. A reset WAL path that overlaps the root is refused except for its
direct contained `<root>/wal`; a default `wal` symlink to a disjoint external
directory remains supported.

A database can move between the two on-disk formats without being stuck in
either. `Database::clone` copies a database into a new store across the full
dir/`.rl` matrix (ADR-0156, ADR-0282): the format of each side is inferred
from the `.rl` suffix on its path, so a native directory clones to a new
directory or a single-file bundle, and a `.rl` bundle clones to a fresh
directory or another bundle. Clone opens the source read-only on the
ADR-0136 safe path (stable-cut manifest retry, bounded reads, fail-closed),
never writes to or exclusively locks the source, refuses an existing
destination, and removes a partially created destination — file or
directory — on failure. The destination keeps the source's snapshot codec
by default; `CloneOptions.to_snapshot_format` instead transcodes the
published checkpoint to a named codec (ADR-0163), failing before the
destination is created when the build does not include that codec.
`CloneOptions.wal_dir` names the source WAL of a directory source and is
rejected for a `.rl` source — a bundle embeds its WAL. A writer-held `.rl`
source fails with `Error::Locked`. See the
[runnable clone round-trip example](examples/clone_roundtrip.md).

Older Native roots with an untyped direct `wal/` cannot be safely reopened or
reset automatically: a closed independent root at that path can have the same
WAL segment layout. The storage crate provides an explicit offline adoption
operation for an operator who has verified the child belongs to the parent.
It takes both exclusive leases and durably marks the existing lock inodes,
leaving WAL and snapshot bytes intact (ADR-0090). See the
[runnable adoption example](examples/adopt_legacy_wal.md).

Recovery bounds untrusted persisted input rather than treating corrupt bytes
as an empty database (ADR-0086). WAL frames are limited to 64 MiB of payload;
a torn tail on any segment, or a final frame on the active segment that fails
its checksum, is omitted after replaying the intact prefix, while interior
corruption still fails recovery. A checksum-valid frame that does not decode
fails WAL open, replay and record reads with `Error::Storage` naming the segment
(`may have been written by a newer Relate`); tolerated-tail trims are
applied only after every segment has been scanned, and the highest segment's
short magic is repaired only after every earlier segment has validated
(ADR-0114). Open also validates the
snapshot's format marker before WAL recovery — Parquet manifest
`format_version` 1 to 4 (4 selects the store's immutable ontology object,
ADR-0120); JSON `value_encoding` absent, 2 or 3 (3 adds the inline ontology
document) with `read_only`
false — and an unsupported marker fails open leaving every WAL segment
unchanged (ADR-0114). On open, WAL segments are scanned one decoded record at a
time for LSN and timestamp summaries; valid commits after the snapshot's
checkpoint LSN are applied directly to the new graph in order, including
pull-identity operations. A genuine (non-tail) recovery error fails open
before that graph is exposed or WAL is recycled. Callers explicitly using the storage crate's
`Wal::replay` receive a materialized `Vec` instead; `Wal::records_after` returns at
most its requested record limit. `.rl` catalogs are limited to 16 MiB and
their page ownership is checked against the host file before reclamation.
Parquet shard metadata and page headers are checked before Arrow page
decompression; actual decoded row/byte totals are counted across batches
before inserting each row.
See [per-shard limits](spec.md#persistent-input-and-replacement-boundaries-adr-0098-adr-0086).
The 1 GiB VFS metadata and open-file checks precede whole-shard allocation,
including for older monoliths. Parquet writers split multi-row shards to fit
the recovery envelope; the 256 MiB default shard target can be overridden by
`RELATE_SHARD_TARGET_BYTES` only within 1..=1 GiB. An invalid override
fails checkpoint. These per-shard limits are not a whole-database memory cap.

Checkpoint replacements use exclusive, unpredictable temporary files and
sync their contents before a close-and-rename (ADR-0098). On Linux/macOS,
Native also syncs the containing directory after rename and syncs newly
created parent entries; a post-rename sync error can leave new bytes visible
while reporting failure. On Windows the replacement is atomic as a namespace
operation, but persistence of the renamed directory entry across sudden power
loss is not promised. Memory is ephemeral; `.rl` updates its virtual catalog
in one host file rather than creating a host temp sidecar for each object.
Each dirty Parquet window publishes new immutable, generation-unique shard
paths (ADR-0089). The old manifest retains byte-identical references until
the replacement manifest is durable; a partial checkpoint failure retains
committed WAL for recovery. Existing fixed-name v3 parts and older v1/v2
monoliths remain readable for v1/v2 manifests. An empty v3 part list means
that kind has no rows, even if a retired monolith remains on disk.
Failed/crashed generations are eligible for orphan cleanup after a validated
recovery; Windows retains ambiguous Native old
generations because a renamed manifest may not survive power loss.
These guarantees do add I/O: each persistent open checks path ancestry and
scans existing WAL trees for nested owners (and a markerless root or external
WAL needs a pre-lock scan), then loads/checks persisted pages and WAL; these
are startup costs, not per-query scans. Each rewritten Native snapshot/manifest
object incurs a temporary-file sync and containing-directory sync on
Linux/macOS, with extra parent syncs for new directories. Bundle checkpoints
use their host-file catalog barriers instead. More dirty shards can therefore
mean more fsync work per checkpoint; the time depends on the filesystem and
number of replaced objects, not a fixed per-database overhead.
`Database::snapshot_data_bytes()` is the size of that snapshot
(`manifest.json` plus Parquet parts and optional pull map, or `graph.json`),
not the WAL. On `:memory:` it stays `0` until an explicit `checkpoint()`.
Metadata failures for present objects propagate, and manifest
and pull-map handles must fit the 16 MiB JSON object limit. Persistent
URIs also dump on a timer (default 5 minutes, ADR-0041) when the WAL has
work. A later `apply_redo` is invisible to an in-flight MATCH. A write-write
conflict is `Error::Conflict`; the engine does not retry (ADR-0017). A
configured memory cap (`OpenOptions.logical_graph_size_limit` / `RELATE_LOGICAL_GRAPH_SIZE_LIMIT`)
rejects growing writes with `Error::Capacity` at WAL append (ADR-0042); the
env var accepts a `SIZE` of bytes or `mb`/`mib`/`gb`/`gib`. A set value that
does not parse fails open with that semantic error. Open and replay
still load a snapshot that is already over the limit.

For Rust applications that exchange persisted `CypherValue` JSON directly,
`relate_model::serde_value` exposes fallible, path-reporting writers and
decoders (ADR-0087). `cypher_value_to_json` and `props_to_json` return
`Result<_, EncodeError>`: malformed/non-finite values fail at the offending
member instead of becoming `null`, and graph entities are rejected as
persistence properties. Time and DateTime also reject constructible
`FixedOffset`s with seconds beyond whole minutes before JSON formatting can
truncate them: a `+00:00:01` offset fails with the full nested value/property
path rather than becoming `+00:00` or changing the instant. Serde serialization
reports the same error. A DateTime must also have a local time representable by
chrono: its UTC instant plus the fixed offset must fit `NaiveDateTime` before
RFC3339 formatting. UTC `NaiveDateTime::MAX` with `+01:00` (or `MIN` with
`-01:00`) returns a path-qualified `EncodeError` rather than panicking, including
inside nested maps/lists and through Serde. Minute-aligned values such as
`+02:30` (with representable local time for DateTime) retain their Time/DateTime
tagged JSON shapes, local time and UTC instant; near-boundary expanded-year
values round-trip in chrono's canonical RFC3339 form. The
model's `VectorCoordKind::is_exact` rule accepts only values the coordinate
kind spells unchanged — finite, bit-preserving conversions, including both
signs of zero — so persistence and every Arrow float-vector kind (FLOAT16 and
BFLOAT16 widened into Float32, FLOAT32, FLOAT64) share one predicate;
`float32_exact` remains as its FLOAT32 convenience alias, not a second rule.

For callers handling numeric timezone text, the public, stateless
`relate_model::serde_value::parse_numeric_offset(&str) ->
Option<chrono::FixedOffset>` accepts only six ASCII bytes in signed `[+-]HH:MM`
form with valid minutes and a chrono-representable offset. It returns `Some`
for `+02:30` and `None` for malformed input such as `+00:60`, multibyte text,
or `Z`, without panicking. Persistence `Time` decoding uses this parser and
reports malformed tagged offsets as decode errors; no database handle is
needed to call the helper. The
[CI-run numeric-offset doctest](examples/decoder_formats.md) demonstrates
both outcomes.

JSON checkpoints write `value_encoding: 2`, which escapes ordinary
maps containing `__type`. Old checkpoints without the field remain readable
under version-1 tag rules, including the unavoidable ambiguity of an old map
that looks exactly like a valid typed value. Malformed known tags and unknown
snapshot encoding versions fail `Database::open` with `Error::Storage` instead
of changing property values. The model decoder can recognize a syntactically
valid tagged Geometry, but core `Value` cannot store it as a property. Opening
either v1 or v2 JSON snapshots with Geometry anywhere in a node/relationship
property (including nested lists and maps) therefore fails with `Error::Storage`
identifying the entity and nested property path, rather than turning it into
`Value::Null`. Point and Vector remain supported and round-trip through JSON
snapshots. The [CI-run snapshot example](examples/decoder_formats.md) opens
valid Point/Vector values and demonstrates the Geometry rejection in both
encodings. A JSON checkpoint with an unencodable property
fails with `Error::Storage` before `graph.json` or its manifest is published,
retaining the last committed snapshot. This includes out-of-range DateTime
local times in nested properties: the error names the entity and complete
property path, a fresh checkpoint publishes nothing, and an existing one
preserves its prior graph and manifest bytes on both Native and Memory VFS.
JSON **query parameters** are a
separate format: all objects stay maps, even when they contain `__type`; use
Cypher constructors for typed values or pass typed Rust values directly. The
[CI-run persistence examples](examples/decoder_formats.md) exercise valid
map/Point writes and a non-finite write error as well as both decode policies.

JSON checkpoints also guard the core-to-model conversion before serialization:
`CivilDate` and `CivilDateTime` properties cannot be persisted losslessly even
when their fields fit chrono; a `Duration` whose `i128` nanoseconds have a
seconds quotient outside `i64` cannot fit the model; and `Time`/`DateTime`
offsets must be minute-aligned and constructible as a fixed offset. This applies
inside list/map properties as well as directly on nodes and relationships.
`Database::checkpoint` returns `Error::Storage` naming the entity ID and full
property path, leaving the prior `graph.json` and manifest intact for recovery
while WAL-committed changes remain available for correction and retry. Valid
Date, LocalTime, Time, LocalDateTime, DateTime with representable local time,
Duration, Point and Vector values still round-trip. The
[CI-run boundary doctest and snapshot example](examples/decoder_formats.md)
cover near-boundary model roundtrips, reopened snapshot values, contextual
failures and preservation of the previous checkpoint.

For Arrow interchange, `relate_arrow` writes explicit field markers for
nodes, relationships, vectors, spatial WKB, and recursively typed composite
JSON (ADR-0088). Its batch reader and WKB readers return `Result` so invalid
metadata, graph identities, WKB, or vector/spatial data cannot quietly become
nulls, IDs, or coordinates. Legacy unmarked structs/lists decode as ordinary
maps/lists; old ambiguous `json`/`tagged` composite columns must be regenerated
to recover graph identity. Full-column inference now exposes one absorbing
`ColType::TypedJson` kind for homogeneous lists/maps/paths, incompatible mixes,
and temporal/duration values outside native Arrow ranges. It replaces the
public `ColType::Json` and `ColType::Mixed` variants: Rust callers matching
either variant should match `TypedJson`. `Null` remains an identity during
inference, and `Int`/`Float` promotes to native `Float64` only when **every**
integer is exactly representable as `f64`. Mixed columns containing an inexact
integer such as `2^53 + 1` or `i64::MAX` use `TypedJson`, preserving each
integer and float rather than rounding it. Fallback columns keep the same
`Utf8` field with `relate.encoding=typed-json-v1` metadata, typed
values and error behavior. Arrow `Time64(Nanosecond)` holds only unzoned Times;
any offset-bearing Time, alone or mixed with unzoned Time, uses `TypedJson` to
preserve each cell's optional offset. A native DateTime timestamp carries a
column-wide fixed offset only when **all** present DateTimes share the same
minute-aligned offset, have no timezone-name hint and fit the native range.
Different offsets or even one named-zone hint use per-cell `DATE_TIME` typed
JSON, preserving each offset, local time, UTC instant and name in single and
chunked batches. Subminute offsets still fail with a column/row and value
path, even after this fallback. The
[CI-run inference doctest](../../../surfaces/relate-arrow/src/schema.rs)
demonstrates a Map and an `Int`/`Str` mix. Arrow typed-JSON
Point/Geometry/Vector payloads reuse the model's fallible tagged-value writer;
invalid nested members carry path-qualified `ArrowError` context. Arrow
timestamp **reader** metadata uses that same model parser for signed offsets:
invalid candidates such as `+00:60` or `-12:99` produce `ArrowError` even on
empty/all-null batches. Arrow separately handles `Z`, case-insensitive `UTC`,
and unsigned named zones, preserving their prior behavior. Batch
writers validate full-result native spatial SRIDs/coordinates and vector
budgets before chunking. Any 3D Point makes the whole spatial column
`typed-json-v1`, including a mix with 2D Points or Geometry across chunks;
per-cell `z` and SRIDs survive. Only 2D Point/Geometry columns stay native
GeoArrow Binary/WKB with a single field SRID. The
[CI-run Arrow doctest](../../../surfaces/relate-arrow/src/from_arrow.rs)
round-trips these fallbacks and an external WKB error. Representable
DateTime/LocalDateTime nanoseconds and Duration month/day/nanosecond components
use checked native conversions; ordinary out-of-range values take a lossless
typed-JSON column, while invalid native plans return `ArrowError`. Native WKB
batch writers preflight finite `x`/`y` through every Geometry descendant before producing
any batches; typed-JSON spatial values use the model writer's finite-coordinate
validation, including `z`. Standalone raw 2D WKB encoders omit Point `z` and
do not run the batch preflight. WKB readers reject external NaN/Infinity,
including Point EMPTY's NaN sentinel, with coordinate/member and array-row
context rather than returning a spatial value. See the
[format contract](spec.md#arrow-values-and-wkb)
and [CI-run Arrow doctest](../../../surfaces/relate-arrow/src/from_arrow.rs)
for valid zoned Time and heterogeneous DateTime round-trips, subminute-offset
errors, and an unsupported legacy-metadata error.

Spatial WKB has just one external, field-level SRID. Before encoding either a
single or chunked batch, the writer checks that all GeometryCollection
descendants match their parent and field SRID; otherwise `ArrowError` names
the column, row and nested member index path. Uniform nested collections
round-trip their child SRIDs and coordinates. The same preflight counts the
top-level WKB header as level 1 and nested headers (including Multi* member
headers) toward a maximum of 64: level 64 is valid and level 65 errors before
writing, even in a later chunk. The native WKB writer and reader share one
Arrow-internal depth constant; it is not an additional public API. Model
persistence JSON can retain collections with differing child SRIDs; the
restriction applies to Arrow WKB's single-SRID field. See
the [CI-run spatial doctest](../../../surfaces/relate-arrow/src/record_batch.rs)
for the round-trip and rejected cases.

Local `LOAD CSV` is a separate, default-denied file capability (ADR-0081).
Pass `OpenOptions.load_csv_dir` for a pre-existing CSV directory or set
`RELATE_LOAD_CSV_DIR`; the explicit option wins. `:memory:` and `.rl`
databases can import files under that root without granting access to the
database directory. Reads are confined to canonical regular files, bounded
per source (8 MiB default, configurable up to 64 MiB), and reused within a
statement; unavailable or oversized files report generic errors. See the
[runnable example](examples/load_csv.md).
The executor's `CsvPolicy::new` validates the same root and limit before a
`Graph` can install the policy; `Database::open_with` constructs it before
opening storage, so invalid settings do not create a WAL or bundle. The
[example](examples/load_csv.md) exercises both the embedded open and the
executor's validated graph configuration.

`CREATE` / `DROP` / `SHOW INDEX` name a `(label, property)` pair in the
catalog (ADR-0040). MATCH still seeks without them.

Every store carries three fixed namespaces (ADR-0116): `neo4j` (the user
data graph and the only writable one), `system` (the CE system database —
SHOW commands, supported administration commands and procedure calls only,
holding no graph data) and `system_ontologies` (visible only under the `RELATE_PREVIEW=ontology` opt-in (ADR-0593; without it the namespace is invisible — unlisted, and every route to the name fails as an unknown database, 42N00)) and not
queryable, ADR-0229: every statement routed to it, reads included, fails
with 42N17, except the ADR-0116 §2 administration commands). A connection's session
namespace (`Database::connect_namespace` —
or `Database::connect_typed` with a typed `Namespace` (ADR-0150) —
`Connection::set_namespace` between transactions) selects where its
statements run (ADR-0117), and each `QueryResult` reports it. The
namespaces are separate information domains: data written on `neo4j` is
invisible to `system` sessions, while the procedure registry, LOAD CSV
policy and memory limit are store-level and shared. See the
[runnable namespace example](examples/namespace.md).

Each `QueryResult` also reports its statement's query type
(`QueryResult.header.query_type`, ADR-0162): the Neo4j Query API v2 `queryType`
classification — `QueryType::Read` (`"r"`), `QueryType::Write` (`"w"`),
`QueryType::ReadWrite` (`"rw"`) or `QueryType::Schema` (`"s"`) — derived
from the logical plan before execution. Because EXPLAIN and PROFILE report
the wrapped statement's type, a caller can route read-only from writing
statements from `EXPLAIN` without executing anything; over Bolt the same
code rides the `type` key of the SUCCESS that ends the result, which Neo4j
drivers read as the summary's query type (ADR-0309).
See the [runnable query-type example](examples/query_type.md).

Parameters and result cells are `relate::Value`s. Strings, lists, maps
and vectors keep their payload behind a shared, immutable `Arc` (ADR-0154,
ADR-0170), so the engine carries a large list or embedding from row to row,
and a caller clones a result, without copying the payload. Build these
values with `.into()` or `Arc::new(..)` and take an owned payload with
`Arc::unwrap_or_clone`. The other surfaces exchange `CypherValue` or
serialized forms, which are unchanged. See the
[runnable value-sharing example](examples/value_sharing.md).

Use this surface from Rust applications and from the CLI. The C ABI is
`relate_*` (`relate-ffi`; ADR-0054).
