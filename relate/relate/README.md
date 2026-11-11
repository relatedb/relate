# relate — Relate for Rust

**An embedded graph database for Rust. Query it in Cypher, in-process — no server, no network, nothing to deploy.**

Relate is an in-memory property-graph engine written in Rust. This crate is
the engine itself — the native embedding API (`relate::Database`): a graph
that is pure in-memory, a directory, or a single `.rl` file, made durable by
a fsync'd write-ahead log and Parquet checkpoints.

> An experimental Neo4j Labs project, not part of Neo4j's supported product
> lineup.

## Install

```bash
cargo add relate
```

Rust 1.88+. The default features include the Parquet snapshot codec;
`default-features = false` builds without Arrow and Parquet (JSON stores,
and a typed refusal when opening a Parquet store — never a silent fallback).

## Quick start

```rust
use std::collections::BTreeMap;

use relate::{Database, Value};

fn main() -> relate::Result<()> {
    let db = Database::open(":memory:")?;   // or a path: Database::open("./friends")
    let mut conn = db.connect()?;

    for (name, age) in [("Alice", 30), ("Bob", 28)] {
        conn.query_with_params(
            "CREATE (:Person {name: $name, age: $age})",
            BTreeMap::from([
                ("name".to_string(), Value::String(name.into())),
                ("age".to_string(), Value::Int(age)),
            ]),
        )?;
    }
    conn.query("MATCH (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'}) \
                CREATE (a)-[:FRIENDS_WITH {since: 2020}]->(b)")?;

    let result = conn.query(
        "MATCH (p:Person)-[:FRIENDS_WITH]->(f) RETURN p.name AS person, f.name AS friend",
    )?;
    assert_eq!(result.header.columns, ["person", "friend"]);
    println!("{} -> {}", result.rows[0][0], result.rows[0][1]);

    Ok(())
}
```

Every auto-commit write appends redo to the WAL, syncs it, and then applies
it to the in-memory graph; opening a store materializes the snapshot plus
WAL replay.

## Persistence

| Store | Open with |
| --- | --- |
| In-memory — gone at exit | `Database::open(":memory:")` |
| Directory — WAL + Parquet shards (default) | `Database::open("./friends")` |
| Single file — WAL, snapshot and catalog in one `.rl` file | `Database::open("./friends.rl")` |
| Read-only — load once, refuse writes and checkpoints | `Database::open_with("./friends", OpenOptions::with_mode(OpenMode::ReadOnly))` |

A second *writable* open of the same store is refused (single-writer lease,
`Error::Locked`); read-only opens can share it.

## Parameters

Pass a `BTreeMap<String, Value>` and reference values as `$name`. `Value` is
the engine's Cypher value: `Null`, `Bool`, `Int`, `Float`, `String`, `List`,
`Map`, temporal and spatial kinds. String, list, map and vector payloads
live behind an `Arc`, so cloning a value — out of a result row or into a
parameter map — never copies the payload.

```rust
conn.query_with_params(
    "UNWIND $rows AS r CREATE (:Item {id: r.id, tags: r.tags})",
    BTreeMap::from([("rows".to_string(), Value::List(vec![
        Value::Map(std::sync::Arc::new(BTreeMap::from([
            ("id".to_string(), Value::Int(1)),
            ("tags".to_string(), Value::List(vec![
                Value::String("a".into()),
                Value::String("b".into()),
            ].into())),
        ]))),
        Value::Map(std::sync::Arc::new(BTreeMap::from([
            ("id".to_string(), Value::Int(2)),
            ("tags".to_string(), Value::List(Vec::new().into())),
        ]))),
    ].into()))]),
)?;
```

## Results

`QueryResult` carries `header.columns`, `rows` (`Vec<Vec<Value>>`), and the
statement's metadata: `header.query_type` (`"r"` / `"w"` / `"rw"` / `"s"`,
derived from the plan — `EXPLAIN` answers it without executing),
`bookmark()` (`"relate:<n>"` after a write, `None` when nothing committed),
and `effect` (`StatementEffect` — whether the statement attempted or applied
a change). Read cells through accessors like `as_string()` or by matching
the `Value` variant.

## Transactions

Statements auto-commit unless a transaction is open.

```rust
conn.begin()?;
conn.query("CREATE (:Person {name: 'Carol'})")?;
conn.query("CREATE (:Person {name: 'Dan'})")?;
let bookmark = conn.commit()?;              // Some("relate:<n>"), or None if nothing committed
```

Dropping the connection rolls an open transaction back.

## Concurrency

Concurrent connections share one database: reads are MVCC snapshots, writes
are optimistic and serialize at WAL append. A write-write conflict is the
typed `Error::Conflict` — the engine does not retry it for you.

## Errors

Every failure is `relate::Error`, a typed enum — `Error::Locked`,
`Error::Conflict`, `Error::Capacity { .. }`, `Error::Storage(..)`, … — so a
retry loop matches variants instead of parsing text. `relate::error_info`
derives the Neo4j status code, GQLSTATUS and compile/runtime/internal stage
when a surface needs them.

## Bigger loads

Send rows in batches with `UNWIND $rows`, a few tens of thousands per
statement, rather than one huge statement or one statement per row.

## Serving it over the network

The same engine runs as a server: start `relate serve --bolt 7687` (see the
[CLI](https://github.com/relatedb/relate/blob/main/docs/api/cli/spec.md))
and use the stock `neo4j` Rust driver against it.

## Documentation

- [API spec](https://github.com/relatedb/relate/blob/main/docs/api/rust/spec.md) — every type, error and lifecycle rule
- [Runnable examples](https://github.com/relatedb/relate/tree/main/docs/api/rust/examples)
- [API documentation](https://docs.rs/relate)
- [Project home](https://github.com/relatedb/relate) · [relatedb.ai](https://relatedb.ai/) · [known limitations](https://github.com/relatedb/relate/blob/main/docs/limitations.md)

## License

Apache-2.0 — see [LICENSE](LICENSE).
