> Repository integration: use the [source build instructions](../README.md).
> The community NuGet package below predates this integration; this branch does not publish it.
> C-string inputs reject embedded NUL; string values preserve it.
> `system_ontologies` requires `RELATE_PREVIEW=ontology` before opening a database.

# Relate.Net User Guide

Relate.Net lets .NET applications embed [Relate](https://github.com/relatedb/relate), an
in-process property-graph database queried with Cypher. This guide covers the whole public API.

- [1. Concepts](#1-concepts)
- [2. Getting started](#2-getting-started)
- [3. Opening databases](#3-opening-databases)
- [4. Connections and namespaces](#4-connections-and-namespaces)
- [5. Running queries](#5-running-queries)
- [6. Parameters](#6-parameters)
- [7. Reading results](#7-reading-results)
- [8. Type mapping](#8-type-mapping)
- [9. Graph values: nodes, relationships, paths](#9-graph-values-nodes-relationships-paths)
- [10. Transactions](#10-transactions)
- [11. Bookmarks](#11-bookmarks)
- [12. Error handling](#12-error-handling)
- [13. Async, threading and disposal](#13-async-threading-and-disposal)
- [14. Diagnostics: plans, notifications, timings](#14-diagnostics-plans-notifications-timings)
- [15. Using Relate.Net from F#](#15-using-relatenet-from-f)
- [16. Using Relate.Net from Visual Basic](#16-using-relatenet-from-visual-basic)
- [17. Limitations](#17-limitations)
- [18. Troubleshooting](#18-troubleshooting)
- [Appendix: API summary](#appendix-api-summary)

---

## 1. Concepts

| Relate.Net type | Wraps | Purpose |
|---|---|---|
| `RelateDatabase` | `relate_database` | An open store: in-memory or on disk. Thread-safe. |
| `RelateConnection` | `relate_connection` | A session that runs Cypher. Serializes its callers. |
| `RelateTransaction` | `relate_conn_begin/commit/rollback` | An explicit transaction on a connection. |
| `QueryResult` | `relate_query_result` | The fully materialized result of one statement plus metadata. |
| `RelateRecord` | one result row | Values by column name or index. Also an `IReadOnlyDictionary<string, object?>`. |
| `RelateException` | the ABI error shape | A relate failure with `Code`, `GqlStatus`, `Stage` and `IsRetryable`. |

The ownership model is simple: **dispose what you open**. The database owns its connections, so
disposing a `RelateDatabase` also disposes every connection opened from it. Results hold **no**
native resources. They are copied into managed objects before `Query` returns, so you can keep them
as long as you like, even after the connection is closed.

## 2. Getting started

```csharp
using Relate.Net;

using var db = RelateDatabase.OpenInMemory();
using var conn = db.Connect();

conn.Query("""
    CREATE (ada:Person {name: 'Ada'}),
           (bob:Person {name: 'Bob'}),
           (ada)-[:KNOWS {since: 2020}]->(bob)
    """);

var result = conn.Query("""
    MATCH (a:Person)-[k:KNOWS]->(b:Person)
    RETURN a.name AS from, b.name AS to, k.since AS since
    """);

foreach (var row in result)
{
    Console.WriteLine($"{row["from"]} knows {row["to"]} since {row.Get<int>("since")}");
}
```

## 3. Opening databases

```csharp
// A fresh, empty, in-memory database (gone when disposed).
using var mem = RelateDatabase.OpenInMemory();            // same as Open(RelateDatabase.InMemory)

// A database stored in a directory (created if missing).
using var disk = RelateDatabase.Open(@"C:\data\graph");

// relate also accepts file:// URIs and *.cl files.
using var uri = RelateDatabase.Open("file:///C:/data/graph");

// Open an existing store without writing, checkpointing or creating it.
using var ro = RelateDatabase.OpenReadOnly(@"C:\data\graph");

// Open on a thread-pool thread.
await using var db = await RelateDatabase.OpenAsync(@"C:\data\graph");
```

| Member | Description |
|---|---|
| `Location` | The location the database was opened from. |
| `IsReadOnly` | `true` for `OpenReadOnly`. Writes and checkpoints then throw `RelateException`. |
| `Checkpoint()` / `CheckpointAsync()` | Flushes the database to its store. |
| `NativeAbiVersion` (static) | The checked C ABI version of the loaded native library (2). |

If a database can't be opened (for example, a read-only open of a missing store), `Open` throws a
`RelateException`.

## 4. Connections and namespaces

```csharp
using var conn = db.Connect();              // session namespace "neo4j"
using var sys  = db.Connect("system");      // "neo4j", "system" or "system_ontologies"

conn.ChangeNamespace("system");             // not allowed while a transaction is open
Console.WriteLine(conn.Namespace);          // "system"

// A USE clause overrides the session namespace for one statement:
var dbs = conn.Query("USE system SHOW DATABASES YIELD name RETURN name");
Console.WriteLine(dbs.Namespace);           // "system"
```

Namespace names are case-insensitive. An unknown name throws a `RelateException` whose message
ends in `Graph not found: <name>`.

All connections of a database see the same graph. Open one connection per concurrent worker (see
[section 13](#13-async-threading-and-disposal)).

## 5. Running queries

```csharp
QueryResult result = conn.Query("MATCH (p:Person) RETURN p.name AS name");
QueryResult result2 = await conn.QueryAsync("MATCH (p:Person) RETURN count(p)", cancellationToken: ct);
```

`Query` always returns a `QueryResult`, including for write-only statements. A failing statement
throws a `RelateException` (see [section 12](#12-error-handling)).

`parameters` is optional. You can use named arguments: `conn.QueryAsync(cypher, cancellationToken: ct)`.

## 6. Parameters

Parameters are referenced as `$name` in Cypher. You can supply them as any of the following:

```csharp
// Anonymous object (C#), the most concise form
conn.Query("RETURN $a + $b", new { a = 1, b = 2 });

// Any IDictionary<string, T>, IReadOnlyDictionary<string, T> or non-generic IDictionary
conn.Query("RETURN $a + $b", new Dictionary<string, object?> { ["a"] = 1, ["b"] = 2 });

// POCOs and records: each public readable property becomes a parameter
public record PersonParams(string Name, int Age);
conn.Query("CREATE (:Person {name: $Name, age: $Age})", new PersonParams("Ada", 36));
```

Nesting is unlimited up to 64 levels. Objects and dictionaries become Cypher maps, and enumerables
become lists:

```csharp
conn.Query("""
    UNWIND $people AS p
    CREATE (:Person {name: p.name, tags: p.tags, address: p.address.city})
    """,
    new
    {
        people = new[]
        {
            new { name = "Ada",   tags = new[] { "math" }, address = new { city = "London" } },
            new { name = "Grace", tags = new[] { "navy" }, address = new { city = "New York" } },
        },
    });
```

### .NET → Cypher parameter types

| .NET value | Cypher type | Notes |
|---|---|---|
| `null`, `DBNull` | `NULL` | |
| `bool` | `BOOLEAN` | |
| `sbyte`, `byte`, `short`, `ushort`, `int`, `uint`, `long` | `INTEGER` | |
| `ulong` | `INTEGER` | Throws `ArgumentOutOfRangeException` above `long.MaxValue`. |
| `float`, `double`, `decimal` | `FLOAT` | `decimal` is converted to `double`. |
| `string`, `char` | `STRING` | Full Unicode. Embedded `\0` is preserved. |
| `Guid` | `STRING` | `"D"` format, as relate represents UUIDs. |
| `enum` | `STRING` | The member name. |
| `DateOnly` | `DATE` | |
| `TimeOnly` | `LOCAL TIME` | |
| `DateTime` (`Kind` = `Utc`) | `ZONED DATETIME` | With offset `+00:00`. |
| `DateTime` (other kinds) | `LOCAL DATETIME` | The wall clock is sent as is. |
| `DateTimeOffset` | `ZONED DATETIME` | With its offset. |
| `CypherZonedDateTime` | `ZONED DATETIME` | With an IANA zone; relate resolves the offset from the zone. |
| `CypherOffsetTime` | `ZONED TIME` | |
| `TimeSpan` | `DURATION` | Seconds and nanoseconds. |
| `CypherDuration` | `DURATION` | Months, days, seconds and nanoseconds. |
| `CypherPoint` | `POINT` | SRIDs 7203, 9157, 4326 and 4979. |
| `CypherVector` | `VECTOR` | Every element type, e.g. `FLOAT32` or `INTEGER8`. |
| `byte[]` | `LIST<INTEGER>` | relate represents bytes as a list of integers. |
| `IDictionary`, `IReadOnlyDictionary`, `IEnumerable<KeyValuePair<string, T>>` | `MAP` | Keys must be strings. |
| other `IEnumerable` | `LIST` | |
| any other object | `MAP` | Built from its public readable instance properties. |

`Node`, `Relationship` and `GraphPath` **cannot** be parameters. Pass the `ElementId` instead and
match on it:

```csharp
var node = conn.Query("CREATE (n:Item) RETURN n")[0].Get<Node>("n");
conn.Query("MATCH (n) WHERE elementId(n) = $id SET n.seen = true", new { id = node.ElementId });
```

If relate rejects a value (an unknown time zone, a vector coordinate the element type can't hold,
a 3D coordinate on a 2D SRID, and so on), `Query` throws an `ArgumentException`. Its message names
the parameter path (for example `people[1].address`), and its `InnerException` is the
`RelateException`.

## 7. Reading results

`QueryResult` is an `IReadOnlyList<RelateRecord>`, so `foreach`, indexing and LINQ all work:

```csharp
var result = conn.Query("UNWIND range(1, 5) AS x RETURN x, x * x AS square");

result.Columns;                     // ["x", "square"]
result.Count;                       // 5
result[0]["square"];                // 1L (object)
result[0][1];                       // 1L, by column index
result[4].Get<int>("square");       // 25

var squares = result.Select(r => r.Get<long>("square")).ToList();
long total = conn.Query("MATCH (n) RETURN count(n)").Scalar<long>();   // first column of first row
```

### RelateRecord

| Member | Description |
|---|---|
| `this[string]`, `this[int]` | The raw value (`object?`). A missing column throws `KeyNotFoundException`, and the message lists the available columns. |
| `Get<T>(string)`, `Get<T>(int)` | The value converted to `T` (see below). |
| `Keys`, `Values`, `Count` | Column names and values, in order. |
| `ContainsKey`, `TryGetValue` | Dictionary-style access. |
| Enumeration | `KeyValuePair<string, object?>` (the record is an `IReadOnlyDictionary<string, object?>`). |

### `Get<T>` conversions

`Get<T>` returns the value unchanged when it already is a `T`. Otherwise it converts:

- **Numbers**: any numeric to any numeric, with overflow checks (`OverflowException`). A number to
  `string` uses the invariant culture.
- **`null`**: allowed for reference types and `Nullable<T>`. A non-nullable value type throws
  `InvalidCastException`.
- **Lists** → `T[]`, `List<T>`, `IList<T>`, `IReadOnlyList<T>`, `IEnumerable<T>`, …, with each
  element converted recursively.
- **Maps** → `Dictionary<string, T>`, `IDictionary<string, T>`, `IReadOnlyDictionary<string, T>`.
- **Strings** → `enum` (case-insensitive), `Guid`, `char`. Integers → `enum`.
- `CypherZonedDateTime` → `DateTimeOffset`. `CypherDuration` → `TimeSpan` (only when it has no
  months). `CypherVector` → `double[]`, `float[]`, `long[]`.

Anything else throws `InvalidCastException`.

### Result metadata

| Member | Description |
|---|---|
| `QueryType` | `Read`, `Write`, `ReadWrite` or `Schema`. |
| `Counters` | `QueryCounters`: `NodesCreated`, `RelationshipsDeleted`, `PropertiesSet`, `IndexesAdded`, … and `ContainsUpdates`. |
| `Bookmark` | The bookmark of an auto-commit write, otherwise `null` (see [section 11](#11-bookmarks)). |
| `Namespace` | The namespace the statement ran in. |
| `Notifications` | Engine notifications (populated by `EXPLAIN`/`PROFILE`). |
| `PlanJson` | The `relate.plan/v1` JSON plan for `EXPLAIN`/`PROFILE`, else `null`. |
| `Timings` | `Elapsed`, `Parse`, `Plan` and `Execute`, as `TimeSpan`s. |

## 8. Type mapping

Values returned by Cypher are materialized as follows:

| Cypher type | .NET type |
|---|---|
| `NULL` | `null` |
| `BOOLEAN` | `bool` |
| `INTEGER` | `long` |
| `FLOAT` | `double` (NaN and ±∞ included) |
| `STRING` | `string` |
| `LIST` | `IReadOnlyList<object?>` |
| `MAP` | `IReadOnlyDictionary<string, object?>` (engine key order) |
| `NODE` | `Node` |
| `RELATIONSHIP` | `Relationship` |
| `PATH` | `GraphPath` |
| `DATE` | `DateOnly` |
| `LOCAL TIME` | `TimeOnly` |
| `ZONED TIME` | `CypherOffsetTime` (`TimeOnly Time`, `TimeSpan Offset`) |
| `LOCAL DATETIME` | `DateTime` (`Kind` = `Unspecified`) |
| `ZONED DATETIME` with offset only | `DateTimeOffset` |
| `ZONED DATETIME` with an IANA zone | `CypherZonedDateTime` (`DateTimeOffset`, `ZoneId`). Converts implicitly to `DateTimeOffset`. |
| `DURATION` | `CypherDuration` (`Months`, `Days`, `Seconds`, `Nanoseconds`) |
| `POINT` | `CypherPoint` (`Srid`, `X`, `Y`, `Z?`) |
| `VECTOR` | `CypherVector` (`ElementType`, `Dimension`, `ToDoubleArray()`, `ToSingleArray()`, `ToInt64Array()`) |
| a kind added by a future relate | `UnsupportedValue` (`KindCode`) |

Notes on fidelity:

- **Precision**: relate keeps nanoseconds, but .NET temporal types have 100 ns ticks. Sub-tick
  digits are truncated when reading. `CypherDuration` keeps full nanoseconds.
- **Range**: .NET dates cover years 1–9999. Reading a Cypher date outside that range throws
  `OverflowException`. So does a `DATETIME` whose offset is not a whole number of minutes, which
  only happens with historical local-mean-time zones.
- **Zones**: send a `CypherZonedDateTime` to keep an IANA zone. A `DateTimeOffset` carries only an
  offset.

### Spatial values

```csharp
var cph = CypherPoint.Wgs84(longitude: 12.57, latitude: 55.68);   // SRID 4326
var p3  = CypherPoint.Cartesian(1, 2, 3);                          // SRID 9157

var km = conn.Query("RETURN point.distance($a, $b) / 1000 AS km",
                    new { a = cph, b = CypherPoint.Wgs84(-0.13, 51.51) }).Scalar<double>();
```

### Vectors

```csharp
var embedding = CypherVector.FromSingles(new[] { 0.12f, -0.4f, 0.33f });     // FLOAT32
var counts    = CypherVector.FromInt64s(new long[] { 1, 2 }, VectorElementType.Integer8);

conn.Query("CREATE (:Doc {embedding: $e})", new { e = embedding });
float[] back = conn.Query("MATCH (d:Doc) RETURN d.embedding").Scalar<float[]>();
```

### Durations

```csharp
conn.Query("RETURN date() + $d", new { d = TimeSpan.FromDays(1) });           // seconds-based
conn.Query("RETURN date() + $d", new { d = new CypherDuration(1, 0, 0, 0) }); // one calendar month

var d = conn.Query("RETURN duration('P1Y2M3DT4H')").Scalar<CypherDuration>();
// d.Months == 14, d.Days == 3, d.Seconds == 14400
```

## 9. Graph values: nodes, relationships, paths

```csharp
var row = conn.Query("""
    CREATE p = (a:Person:Dev {name: 'Ada'})-[k:KNOWS {since: 2020}]->(b:Person {name: 'Bob'})
    RETURN a, k, p
    """)[0];

Node ada = row.Get<Node>("a");
ada.ElementId;               // "4:rl-id:n0", identical to Cypher elementId(n)
ada.Id;                      // the deprecated numeric id(n)
ada.Labels;                  // ["Person", "Dev"]
ada.HasLabel("Dev");         // true
ada["name"];                 // "Ada"
ada.Get<string>("name");     // "Ada"
ada.Properties;              // IReadOnlyDictionary<string, object?>

Relationship knows = row.Get<Relationship>("k");
knows.Type;                  // "KNOWS"
knows.StartNodeElementId;    // == ada.ElementId
knows.EndNodeElementId;
knows["since"];              // 2020L

GraphPath path = row.Get<GraphPath>("p");
path.Length;                 // 1 (hops)
path.Start; path.End;        // Node
path.Nodes; path.Relationships;
```

`Node` and `Relationship` compare equal when their `ElementId`s match.

## 10. Transactions

Statements outside a transaction auto-commit. For several statements that must succeed or fail
together, use an explicit transaction:

```csharp
using (var tx = conn.BeginTransaction())
{
    tx.Query("CREATE (:Account {id: 1, balance: 100})");
    tx.Query("CREATE (:Account {id: 2, balance: 0})");
    Bookmark? bookmark = tx.Commit();
}   // disposing an uncommitted transaction rolls it back
```

- `Commit()` returns the transaction's `Bookmark`, or `null` if the transaction wrote nothing.
- `Rollback()` discards the transaction's writes. Disposing an active transaction does the same,
  and never throws.
- While a transaction is open, statements run through **either** `tx.Query` **or** `conn.Query`
  belong to it.
- A connection can have only one open transaction. A second `BeginTransaction` throws
  `InvalidOperationException`. `conn.CurrentTransaction` returns the open one.
- Using a transaction after commit or rollback throws `InvalidOperationException`.
- **When a statement fails inside a transaction**, relate marks the transaction as requiring
  rollback. Further statements fail with "transaction failed; rollback required". Let `using`
  dispose the transaction, or call `Rollback()`, and retry. If `Commit()` fails, Relate.Net rolls
  the transaction back before throwing.
- Writes in an open transaction aren't visible to other connections until the commit.
- Disposing the connection ends any open transaction without committing it.

There are async versions of each method: `BeginTransactionAsync`, `QueryAsync`, `CommitAsync`,
`RollbackAsync` and `await using`.

### A retry pattern

```csharp
for (int attempt = 1; ; attempt++)
{
    try
    {
        using var tx = conn.BeginTransaction();
        tx.Query("MATCH (a:Account {id: 1}) SET a.balance = a.balance - 10");
        tx.Query("MATCH (a:Account {id: 2}) SET a.balance = a.balance + 10");
        tx.Commit();
        break;
    }
    catch (RelateException ex) when (ex.IsRetryable && attempt < 3)
    {
        // the using block has already rolled back; try again
    }
}
```

## 11. Bookmarks

A bookmark (`relate:<n>`) identifies the store's commit sequence after a write. You get one from
`tx.Commit()` or from `result.Bookmark` after an auto-commit write. Sequence numbers increase
across every connection, so a later commit always has a greater bookmark:

```csharp
Bookmark first  = conn.Query("CREATE (:A)").Bookmark!;
Bookmark second = other.Query("CREATE (:B)").Bookmark!;
bool later = second > first;                       // true; compares Sequence, not text
Bookmark parsed = Bookmark.Parse("relate:42");     // also Bookmark.TryParse
long n = parsed.Sequence;                          // 42
```

`Bookmark` implements `IComparable<Bookmark>` and `IEquatable<Bookmark>` and overloads the
comparison operators. A naive string comparison would get this wrong, because "relate:10" sorts
before "relate:9".

## 12. Error handling

| Exception | When |
|---|---|
| `RelateException` | relate reported a failure: syntax, semantic or runtime error, unknown namespace, read-only violation, IO/storage error, … |
| `ArgumentException` | A parameter value was rejected (inner exception: `RelateException`) or can't be represented. |
| `ArgumentNullException` | A `null` Cypher string or location. |
| `ObjectDisposedException` | Use of a disposed database or connection. |
| `InvalidOperationException` | Transaction misuse (nested begin, use after commit), or `Scalar` on an empty result. |
| `InvalidCastException`, `OverflowException` | `Get<T>` couldn't convert the value. |
| `PlatformNotSupportedException` | The native library does not match this process architecture. |
| `DllNotFoundException` | The platform's native Relate library could not be loaded. |

`RelateException` carries relate's structured error:

```csharp
try
{
    conn.Query("RETURN 1 / 0");
}
catch (RelateException ex)
{
    ex.Message;      // "type error: / by zero"
    ex.Code;         // "Neo.ClientError.Statement.ArithmeticError"
    ex.GqlStatus;    // "22G03"
    ex.Stage;        // ErrorStage.Runtime   (Compile | Runtime | Internal)
    ex.IsRetryable;  // false
}
```

After a failed auto-commit statement the connection stays usable. For storage and internal errors
relate deliberately returns the fixed message "internal error". Set the environment variable
`RELATE_FFI_LOG=1` before the process starts to have relate log the redacted detail to stderr.

## 13. Async, threading and disposal

**Async.** Every potentially slow operation has an async version: `OpenAsync`, `CheckpointAsync`,
`QueryAsync`, `BeginTransactionAsync`, `CommitAsync`, `RollbackAsync` and `DisposeAsync`. The
native engine is synchronous, so async calls run the native work on the thread pool. A
`CancellationToken` is honoured while waiting for the connection and before the statement starts.
A statement that has started runs to completion.

**Threading.**

- A `RelateDatabase` can be shared freely between threads.
- A `RelateConnection` can be shared too. It serializes its callers, so only one statement runs at
  a time on that connection.
- For real parallelism, open one connection per worker:

```csharp
await Parallel.ForEachAsync(items, async (item, ct) =>
{
    await using var c = db.Connect();
    await c.QueryAsync("MERGE (:Item {id: $id})", new { id = item.Id }, ct);
});
```

**Disposal.**

- `RelateDatabase.Dispose()` disposes all its connections, then closes the store.
- `RelateConnection.Dispose()` ends any open transaction and closes the session.
- `RelateTransaction.Dispose()` rolls back an active transaction.
- Every `Dispose` is idempotent. Native handles are wrapped in `SafeHandle`s, so a leaked object is
  still released by the finalizer. A connection keeps its database's native handle alive.
- `QueryResult` and the values in it need no disposal.

## 14. Diagnostics: plans, notifications, timings

```csharp
var explain = conn.Query("EXPLAIN MATCH (a:Person), (b:Person) RETURN a, b");

foreach (var n in explain.Notifications)
    Console.WriteLine($"{n.Severity} {n.Code}: {n.Title} - {n.Remedy}");

using var plan = System.Text.Json.JsonDocument.Parse(explain.PlanJson!);   // relate.plan/v1

var t = conn.Query("MATCH (n) RETURN count(n)").Timings;
Console.WriteLine($"total {t.Elapsed.TotalMilliseconds} ms (parse {t.Parse.TotalMilliseconds}, plan {t.Plan.TotalMilliseconds}, exec {t.Execute.TotalMilliseconds})");
```

Use `PROFILE` instead of `EXPLAIN` to run the statement and include actual row counts in the plan.

## 15. Using Relate.Net from F#

```fsharp
open Relate.Net

use db = RelateDatabase.OpenInMemory()
use conn = db.Connect()

// F# Map, dict, or anonymous records as parameters
conn.Query("CREATE (:Person {name: $name, tags: $tags})", {| name = "Ada"; tags = [ "math" ] |}) |> ignore
let total = conn.Query("RETURN $a + $b", Map.ofList [ "a", 40; "b", 2 ]).Scalar<int64>()

// Pattern matching on values
for record in conn.Query("MATCH (p:Person) RETURN p") do
    match record.["p"] with
    | :? Node as n -> printfn "%s" (n.Get<string>("name"))
    | _ -> ()

// Transactions: `use` rolls back unless committed
do
    use tx = conn.BeginTransaction()
    tx.Query("CREATE (:Temp)") |> ignore
    tx.Commit() |> ignore

// Async
let countAsync () = task {
    let! r = conn.QueryAsync("MATCH (n) RETURN count(n)")
    return r.Scalar<int64>()
}
```

With nullable reference types enabled, F# sees record values as `objnull`.

## 16. Using Relate.Net from Visual Basic

```vb
Imports Relate.Net

Using db = RelateDatabase.OpenInMemory(), conn = db.Connect()
    conn.Query("CREATE (:Person {name: $name})", New With {.name = "Ada"})

    For Each record In conn.Query("MATCH (p:Person) RETURN p.name AS name")
        Console.WriteLine(record.Get(Of String)("name"))
    Next

    Using tx = conn.BeginTransaction()
        tx.Query("CREATE (:Temp)")
        Dim bookmark As Bookmark = tx.Commit()
    End Using

    Dim result = Await conn.QueryAsync("RETURN 1 AS one")   ' inside an Async method
End Using
```

## 17. Limitations

- Native platforms: Windows x64; Linux x64/ARM64; macOS x64/ARM64. Build from the matching checkout; see the client README.
- Results are fully materialized in memory. Use `SKIP`/`LIMIT`, or aggregate in Cypher, for very
  large result sets.
- A running statement can't be cancelled. Cancellation tokens apply only before it starts.
- Relate.Net is built against relate C ABI v2 and refuses to load another major ABI version.
- relate imposes fixed limits on parameters: 100,000 entries per list or map, 4096-byte parameter
  names and a 16 MiB total parameter budget per statement. Exceeding them throws an
  `ArgumentException`.

## 18. Troubleshooting

| Symptom | Fix |
|---|---|
| `DllNotFoundException` | Build the native library from this checkout with `cargo build -p relate-ffi`. The client copies the host library beside `Relate.Net.dll`; override its source with `-p:RelateNativeLibraryPath=…`. Packages use `runtimes/<rid>/native`. No native release is downloaded. |
| `BadImageFormatException` | The process is 32-bit or Arm64. Run as x64 (`<PlatformTarget>x64</PlatformTarget>` or `<RuntimeIdentifier>win-x64</RuntimeIdentifier>`). |
| `PlatformNotSupportedException` | Check that the native library matches this process architecture. |
| "transaction failed; rollback required" | A statement failed earlier in the same transaction. Roll back and retry. |
| `RelateException` with message "internal error" | Set `RELATE_FFI_LOG=1` to see relate's redacted diagnostics on stderr. |

---

## Appendix: API summary

```text
namespace Relate.Net

RelateDatabase : IDisposable, IAsyncDisposable
    const InMemory = ":memory:", DefaultNamespace = "neo4j"
    static int NativeAbiVersion
    static Open(location) / OpenInMemory() / OpenReadOnly(location) / OpenAsync(location, ct)
    Location, IsReadOnly, IsDisposed
    Connect(namespace = null) -> RelateConnection
    Checkpoint() / CheckpointAsync(ct)

RelateConnection : IDisposable, IAsyncDisposable
    Database, Namespace, CurrentTransaction, IsDisposed
    Query(cypher, parameters = null) -> QueryResult
    QueryAsync(cypher, parameters = null, ct = default) -> Task<QueryResult>
    BeginTransaction() / BeginTransactionAsync(ct) -> RelateTransaction
    ChangeNamespace(namespace)

RelateTransaction : IDisposable, IAsyncDisposable
    Connection, IsActive
    Query / QueryAsync
    Commit() / CommitAsync(ct) -> Bookmark?
    Rollback() / RollbackAsync(ct)

QueryResult : IReadOnlyList<RelateRecord>
    Columns, QueryType, Namespace, Counters, Notifications, PlanJson, Bookmark, Timings
    Scalar<T>()

RelateRecord : IReadOnlyDictionary<string, object?>
    this[string], this[int], Get<T>(string), Get<T>(int), Keys, Values

Node, Relationship, GraphPath, UnsupportedValue
CypherPoint, CypherDuration, CypherOffsetTime, CypherZonedDateTime, CypherVector, VectorElementType
QueryCounters, Notification, QueryTimings, QueryType, Bookmark
RelateException (Code, GqlStatus, Stage, IsRetryable), ErrorStage
```
