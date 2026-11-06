# Swappable engines

Relate is split into two engines that can be replaced one at a time. The
**query engine** runs planned statements and owns the layout of the graph
and its indexes. The **write engine** makes changes durable and owns the
commit clock. Today there is one of each: the in-memory query engine and
the local write engine. This document explains how the two fit together,
what crosses the boundary, and what a second engine has to supply.

The decisions behind it are recorded in
[ADR-0526](../adr/0526-query-engine-seam.md) (query engine) and
[ADR-0527](../adr/0527-write-engine-seam.md) (write engine).

## Why two engines

The current engines make Relate an in-memory, single-instance, ACID
database. Other shapes are plausible later: a clustered write engine, a
columnar or paged query engine for analytics, or experimental storage
formats. Each of those changes one half and leaves the other alone. A
clustered log still feeds the same redo into the same graph. A paged
graph is still checkpointed by the same log. Splitting at that line
means work on one engine cannot break the other.

The language stays shared. A second engine never reimplements Cypher.
This is the difference from the withdrawn second runtime
([ADR-0052](../adr/0052-graphstore-withdrawn.md)), which split language
coverage between two implementations.

## Crate layout

```text
relate                facade: Database, Connection, admission, apply, publish
├── shared front      relate-parser, relate-front, relate-ir, relate-planner
├── relate-engine-api contracts both engines speak
├── relate-executor   in-memory query engine
└── relate-write-local local write engine
    └── relate-storage VFS, log record format, snapshot codecs
```

| Crate | Role |
| --- | --- |
| `relate-engine-api` | Traits and shared types. Names no concrete graph or log. |
| `relate-executor` | `InMemoryQueryEngine`: execution plus the in-memory graph and indexes. |
| `relate-write-local` | `LocalWriteEngine`: the process-local log, checkpoint, and follow. |
| `relate-storage` | The toolkit a write engine builds on: VFS, log format, snapshot codecs, recovery. |
| `relate` | The facade. The only crate that names both concrete engines, in `src/engines.rs`. |

Two dependency rules keep the halves apart:

- `relate-storage` and `relate-write-local` never take a normal or build
  dependency on `relate-executor`. The boundary checker
  (`testing/xtask/src/boundaries.rs`, rule R8) refuses it. Dev
  dependencies are allowed so storage tests can build a real graph.
- Inside the facade, only `src/engines.rs` names `InMemoryQueryEngine` and
  `LocalWriteEngine`. Everything else holds the facade's engine types.

## The shared contracts

All of these live in `relate-engine-api`. They were moved there, not
redesigned, and the old paths re-export them.

**`RedoOp`** is the logical change unit. A statement's writes become a list
of redo operations. The same list is the log payload, the follower's
replication unit, and the change-data-capture feed. Any query engine must
produce it and apply it. Any write engine must store and replay it.

**`CommitRecord`** is one durable group: `lsn`, `commit_ts`, and its `ops`.
It replaces the storage crate's old `WalRecord` with the same fields and
the same JSON encoding. `relate_storage::WalRecord` and the public
`relate::WalRecord` are aliases for it.

**`CheckpointTheater` and `SchemaSnapshot`** are the catalog image a
snapshot carries: databases, aliases, and the SHOW metadata.

**`GraphExport` and `GraphImport`** are what a snapshot codec may ask of a
graph, and what recovery may put back:

- `GraphExport` reads the commit timestamp, id counters, the nodes and
  relationships visible at a timestamp, the ids dirtied since the last
  checkpoint, the catalog, and per-entity checkpoint sizes.
- `GraphImport` sets counters and the commit timestamp, loads nodes and
  relationships, installs indexes and constraints, applies replayed redo,
  and finishes the restore.

Every snapshot codec takes `&dyn GraphImport`. The open and load
functions are generic over `SnapshotGraph` (`GraphImport + Default`), so
recovery builds whatever graph type the caller names. That is what lets
any query engine be checkpointed by any write engine.

## The query engine

`QueryEngine` is the facade-facing trait. It is deliberately coarse: one
call per statement or per commit, never per row, so an engine's inner
loop stays statically dispatched.

- `Scratch`: the engine's private transaction state. For the in-memory
  engine this is the write overlay, `RefCell<WriteScratch>`.
- `Output`: the result type. The in-memory engine returns its
  `QueryResult`, which the facade converts once.
- `execute_read` and `execute_write` run a plan the facade has already
  planned and admitted.
- `take_redo` drains the redo a write produced.
- `validate_redo_group` and `apply_redo` check and apply a committed
  group in timestamp order.
- `snapshot_pinned` and `gc_versions` let the facade coordinate reloads
  and version cleanup.
- `pattern_stats(params)` returns the planner's cost input for one
  statement. The planner is shared, so this is how it learns the cost of
  a layout it does not know.

A query engine must also implement `GraphExport` and `GraphImport`.

The in-memory engine wraps the existing `Graph`. It dereferences to the
graph, so the facade's existing graph calls still compile. Its own
`execute(view, request)` method takes the full executor request,
including the PROFILE metrics sink and the batch-commit sink that
`CALL { } IN TRANSACTIONS` uses. The facade calls that method for every
statement. The trait's `execute_read` and `execute_write` do not carry
those sinks yet.

## The write engine

`WriteEngine` owns durable ordering:

- `commit(groups)` makes a list of redo groups durable and returns the
  `lsn` and `commit_ts` of each.
- `checkpoint(graph, lsn, snapshot_ts)` publishes a snapshot taken
  through `GraphImport`, covering the log through `lsn`.
- `poll_follow(from_lsn)` returns records past a follower's cursor:
  `Idle`, `Records`, or `Gap`, which means reload from the snapshot.
- `reload(graph)` reopens the log against a freshly loaded graph.

The trait is object safe and spawns no threads. Checkpoint timers and
follow loops stay in the facade, which compiles them out on `wasm32`.

The local write engine wraps the existing `Wal` and store target. Its
`append_assigned` function preflights and appends a group of records that
already carry their timestamps, without a durability barrier. The
facade's commit pipeline calls it while it holds the log lock, then
decides the barrier from the durability mode.

## What the facade keeps

The write engine replaces only the durable step. The facade keeps
everything that orders a commit against readers, because visibility and
snapshot pins belong to the query engine:

1. **Admission.** Write-set conflict checks against the snapshot,
   constraint-key admission, the capacity reservation, and the commit
   timestamp for each slot in a group.
2. **Durable append.** Through the write engine, under the log lock and
   the publication lock.
3. **Apply.** `apply_redo` on the query engine, in timestamp order.
4. **Publication.** Plan-cache invalidation and the release of the
   publication lock.

Group commit stays one leader per batch, with one durability barrier per
group. A clustered write engine would replace step 2 and leave the
others alone.

The live graph sits in `GraphCell`, which a follower can replace under the
exclusive publication lock once no snapshot is pinned. The cell now holds
the query engine instead of the bare graph. The replacement rule and
the lock ordering are unchanged.

## Selection and store identity

`src/engines.rs` holds two enums, one variant each:

```rust
pub(crate) enum LiveQuery { InMemory(InMemoryQueryEngine) }
pub(crate) enum LiveWrite { Local(LocalWriteEngine) }
```

Both dereference to their one variant, so call sites stay unchanged until
a second variant exists. Dispatch is static. Nothing is boxed, and no
scratch type goes through `dyn`.

Every checkpoint records the pair that wrote it. The Parquet manifest and
the JSON envelope both gain two optional fields:

```json
{ "query_engine": "in-memory", "write_engine": "local" }
```

A store written before this change has neither field, and it reads as
`in-memory` plus `local`. Opening a store whose recorded pair is not one
this build knows fails with a storage error naming both pairs. Older
readers ignore the unknown fields, and redo and snapshot payloads keep
their encoding.

The selector is crate-private. `OpenOptions` does not change, and no
public API exposes an engine choice yet.

## Testing the seam

- The boundary rule R8 has its own unit tests: a normal dependency from a
  write-engine crate onto the query engine is refused, and a dev
  dependency is allowed.
- `testing/relate-test-support` has `RecordingWriteEngine`, which wraps
  any `WriteEngine`, records every call, and forwards it. Its test drives
  a commit and a follow poll through the trait against the local engine.
- The manifest tests check that a legacy manifest reads as the historical
  pair and that a different engine id is refused.
- The cookbook runner, the memory-controls serve child, and the
  spec-harness lane each name the one engine pair they run. A second
  pair adds a row instead of a new harness.
- `gosonic ci run --jobs 10 full-test` is the gate for behavior. It
  includes the follow tests that guard the `GraphCell` reload ordering.

## Current limits

The traits are in place, but the facade still reaches the engines
through `Deref` in several places. Adding a second engine means closing
these gaps first:

- **Query execution.** The facade calls the in-memory engine's own
  `execute` method, not `QueryEngine::execute_read` or `execute_write`,
  because the trait requests do not carry the PROFILE and batch-commit
  sinks. Moving those sinks into `relate-engine-api` is the next step.
- **Graph calls.** Admission, snapshot pins, procedures, and services
  still call `Graph` methods through the deref chain. Each needs a trait
  method, or must move into the engine.
- **Write path.** Commit appends go through
  `relate_write_local::append_assigned`. Checkpoint scheduling, follow,
  CDC slots, and the writer lease are still facade code. `checkpoint.rs`
  and `follow.rs` use the log handle directly, and CDC slots take their
  log position from the facade. They need to move behind `WriteEngine`.
- **Recording engine coverage.** The recording engine is tested on its
  own. It is not yet installed under the facade's commit, transaction,
  and CDC suites. That waits on the write-path move above.
- **Shared evaluation.** Expression evaluation and the function and
  procedure registries live inside `relate-executor`. A second query
  engine needs them in a shared crate, as ADR-0526 records.

## Adding an engine later

A new query engine is its own crate that implements `QueryEngine`,
`GraphExport`, and `GraphImport`, supplies `PatternStats`, and adds a
`LiveQuery` variant and an engine id behind a cargo feature. A new write
engine is its own crate that implements `WriteEngine`, can reuse the
`relate-storage` toolkit, and adds a `LiveWrite` variant and an id. The
existing engines need no edits, and the new pair joins the engine-pair
lists the test runners already read.
