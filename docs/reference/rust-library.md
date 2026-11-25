# Rust Library Reference

Embed Relate with `relate::Database` (WAL + Relate parser).
GraphStore / `relate-engine` is withdrawn (ADR-0052). Python and the
C FFI (`relate_*`) use the same runtime.
Bolt language clients use the product C ABI in `relate-ffi`.

## Quick start

```rust
use std::collections::BTreeMap;
use relate::{Database, Value};

let db = Database::open(":memory:").unwrap();
let mut conn = db.connect().unwrap();

conn.query_with_params(
    "CREATE (:Person {name: $name, age: $age})",
    BTreeMap::from([
        ("name".into(), Value::String("Alice".into())),
        ("age".into(), Value::Int(30)),
    ]),
)
.unwrap();

let result = conn.query("MATCH (n:Person) RETURN n.name AS name, n.age AS age").unwrap();
for row in &result.rows {
    println!("{:?}", row);
}
```

## Snapshot codecs

Persistent URIs are **directories**. WAL segments carry the `MDBWAL01` magic, or `MDBWAL02` for segments created while the store holds an ontology ([ADR-0120](https://github.com/relatedb/relate/blob/main/docs/adr/0120-ontology-document-persistence.md) §4: the first committed `db.ontology.set` rotates the WAL to `MDBWAL02`; after a drop, a checkpoint and WAL recycling, new segments are `MDBWAL01` again); the snapshot is one codec per directory (ADR-0050). Empty URI: `OpenOptions.snapshot_codec`, else `RELATE_SNAPSHOT_FORMAT`, else Parquet (JSON in a build without the `parquet` feature; see [Cargo features](#cargo-features)). Existing directories follow `manifest.json` / files; a manifest without a `codec` field means Parquet.

**Store versions and migrate-on-open** ([ADR-0127](https://github.com/relatedb/relate/blob/main/docs/adr/0127-store-version-compatibility.md)): every durable subsystem carries a version marker — Parquet manifest `format_version` (writes 3, or 4 while the store holds an ontology; reads 1–4), JSON `graph.json` `value_encoding` (writes 2, or 3 while the store holds an ontology; reads absent/1, 2 or 3), WAL segment magic (writes `MDBWAL01`, or `MDBWAL02` while the store holds an ontology; reads both), and the `.rl` bundle superblock (`MDBCLB01`, `VERSION` 1, frozen). The ontology-dependent markers follow the content (ADR-0120 §8): a store that never sets an ontology is byte-for-byte in the v3/encoding-2/`MDBWAL01` format, and a store returns to it after the ontology is dropped, a checkpoint publishes and the `MDBWAL02` segments are recycled; older binaries fail closed on an ontology-carrying store before modifying any data. One registry in `relate-storage` (`src/version.rs`) names the current and readable markers per subsystem, with one legacy reader per readable version plus the current writer; detection is the marker the codec loader already decoded, so a current-format open reads each snapshot file exactly once. Opening a store whose markers are readable but not current migrates it in place, hands-off, before the first query is served, in one open flow: legacy-reader load, `Wal::open`, WAL replay, then — on a durable VFS only — checkpoint at the current format and WAL rotation to a fresh current-magic segment on the handles already open (no second load). A second open performs no migration; `:memory:` never migrates. Migration is **forward-only** — a migrated store is rejected by older binaries — and a store written by a **newer** Relate hard-fails at open before any byte mutates (ADR-0114). A crash mid-migration leaves the old store intact and the next open re-migrates. There is no user-facing migration API; the report type is internal (`pub(crate)`) and no log line is emitted.

```rust
use relate::{Database, OpenOptions, SnapshotCodecKind};

// Default (with the `parquet` feature): parquet shards + manifest.json + wal/
let parquet = Database::open("./graph").unwrap();
parquet.checkpoint().unwrap();

let json = Database::open_with(
    "./graph-json",
    OpenOptions {
        snapshot_codec: Some(SnapshotCodecKind::Json),
        ..Default::default()
    },
)
.unwrap();
json.checkpoint().unwrap(); // writes graph.json (full GraphData rewrite)
```

Import a Relate GraphData file: copy it to `<dir>/graph.json` and open with `SnapshotCodecKind::Json`.

## Durability modes

`OpenOptions::durability_mode: None` uses `RELATE_DURABILITY_MODE` (`full`/`everysecond`/`snapshot`) or defaults to `DurabilityMode::Full` (ADR-0470); `Some(mode)` wins.
`EverySecond` acknowledges after in-memory apply and accepts loss of the latest
approximately one second of writes in a crash; a delayed barrier can extend
that interval. `Snapshot` accepts loss of all writes since the latest completed
checkpoint. Timed checkpoints default to 300 seconds; set
`checkpoint_interval`, where `Duration::ZERO` disables only the timer.

## Adding it to your project

Relate's crates are **not yet published to [crates.io](https://crates.io/)**
(the workspace is `1.3.1`). Until they are, an external project depends on
them with a **git dependency**:

```toml
[dependencies]
relate = { git = "https://github.com/relatedb/relate", tag = "v1.3.1" }
```

Pin `tag` (or `rev`) so your build is reproducible; omit it to track the default
branch. Once the crates are published, this becomes a plain version dependency:

```toml
# after crates.io publication (not yet available)
relate = "1.3.1"
```

:::note
The relative-path form used in this repository —
`relate = { path = "relate/relate" }` — **only resolves
inside the Relate workspace**. External projects should use the git
dependency above.
:::

### Cargo features

`relate` has one Cargo feature, `parquet`, on by default (ADR-0105). It
compiles the Parquet snapshot codec and its Arrow/Parquet dependencies. To
build a leaner engine without them:

```toml
[dependencies]
relate = { git = "https://github.com/relatedb/relate", default-features = false }
```

The feature is not in any release yet: v1.3.1 and earlier always compile
Parquet, so `default-features = false` changes nothing there. Track the
default branch as above, or pin a `rev` or a later tag that includes ADR-0105.

Without `parquet`, new stores use the JSON codec. Opening a Parquet store or
requesting `SnapshotCodecKind::Parquet` fails at open with
`Error::Storage("parquet snapshot codec is not compiled into this build (enable the \"parquet\" feature)")`.
The full contract is in the
[Rust spec](https://github.com/relatedb/relate/blob/main/docs/api/rust/spec.md#cargo-features).

Engine-book contracts live in [`docs/api/rust`](https://github.com/relatedb/relate/blob/main/docs/api/rust/spec.md).

## Related crates

| Crate | Role |
| --- | --- |
| `relate` | Public `Database` / `Connection` |
| `relate-front` | Relate parse → planner IR |
| `relate-ir` | Planner AST (not a second query parser) |
| `relate-planner` / `relate-executor` | Plan and execute |
| `relate-storage` | WAL + parquet/json snapshots (Parquet behind the default `parquet` feature) |
| `relate-model` | Shared value types and wire JSON |
| `relate-arrow` | Arrow IPC for Python |
| `relate-ffi` | Product `relate_*` C ABI |
