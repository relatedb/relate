# .NET client — specification

Namespace and assembly: `Relate.Net`. Target: .NET 8 or later. Build with the
.NET 10 SDK; the F# tests require its compiler. Native contract:
[C ABI v2](../c/spec.md), using source-generated P/Invoke, UTF-8 strings and
owned safe handles. The runtime rejects a different ABI major before opening
a database. The client has no managed runtime package dependencies.

## Build, packaging and platforms

Follow the [build instructions](../../../clients/dotnet/README.md). `cargo build
-p relate-ffi` supplies the current checkout's native library; .NET copies it
into output directories. `RelateNativeLibraryPath` overrides the default
`target/debug` native file. `RelateNativeRuntimeIdentifier` overrides the SDK
host RID used in a local package's `runtimes/<rid>/native` asset. Supported RIDs
are `win-x64`, `linux-x64`, `linux-arm64`, `osx-x64`, `osx-arm64`, and the file
name each RID ships is that list's one table. A local package contains one native
platform; `RelateNativeDistDir` names a directory holding `<rid>/<library>` for
every supported RID and packs all five into one package instead — the mode the
release uses.

## Install

```sh
dotnet add package Relate.Net
```

The package ID is the adopted client's community package; this repository shares
ownership of it and publishes its releases there, carrying all five RIDs'
natives. Published versions equal the release tag: the release sweep bumps the
project's `<Version>` and the release's package check refuses a mismatch.

CI runs the C#, F# and VB tests and this directory's executable example against
native code built from the same commit. .NET 8 runs the tests; newer compatible
runtimes may consume the library. Native loading follows .NET's standard library
resolution. No release download or custom extraction occurs at runtime.

## Lifecycle and execution

| Type / member | Contract |
| --- | --- |
| `RelateDatabase.InMemory`, `DefaultNamespace` | `:memory:` and `neo4j`. |
| `Open(location)`, `OpenInMemory()`, `OpenReadOnly(location)` | Open/create a store, create an isolated memory store, or open an existing store without writes. Empty/null locations fail. |
| `OpenAsync(location, cancellationToken)` | Schedules `Open` on the thread pool. Cancellation applies before execution. |
| `Location`, `IsReadOnly`, `IsDisposed`, `NativeAbiVersion` | Opening location/mode, disposed state, and checked native ABI major. |
| `Connect(namespace = null)` | Create a session; null selects `neo4j`. Namespace matching is case-insensitive. |
| `Checkpoint()`, `CheckpointAsync(token)` | Flush a durable store; no-op for memory stores, error for read-only stores. |
| Database `Dispose()`, `DisposeAsync()` | Idempotently close all owned connections, then the native database. |
| `RelateConnection.Database`, `Namespace`, `CurrentTransaction`, `IsDisposed` | Owning database, session default, current transaction or null, and disposed state. |
| `Query(cypher, parameters = null)`, `QueryAsync(cypher, parameters = null, cancellationToken)` | One Cypher statement; return a fully materialized `QueryResult`. |
| `ChangeNamespace(namespace)` | Change the session default between transactions; unknown namespaces fail. |
| `BeginTransaction()`, `BeginTransactionAsync(token)` | Start one explicit transaction; a second active transaction fails. |
| Connection `Dispose()`, `DisposeAsync()` | Idempotently disconnect and roll back an active transaction. |
| `RelateTransaction.Connection`, `IsActive` | Owner and active state. |
| Transaction `Query`, `QueryAsync` | Same parameter/result contract; reject a completed transaction under the connection lock, including queued calls. |
| `Commit()`, `CommitAsync(token)` | Finish the transaction, returning its bookmark or null for no mutation. A failed commit throws and rolls back. |
| `Rollback()`, `RollbackAsync(token)` | Discard writes and complete the transaction. |
| Transaction `Dispose()`, `DisposeAsync()` | Roll back if still active; suppress expected disposal/transaction errors. |

A database can be shared across threads. Every connection serializes its native
operations; parallel workers use separate connections. The database remains
alive until native children are closed. Connect/dispose races return a closed
connection or throw `ObjectDisposedException`, never leave an orphaned session.
Calls after disposal fail. Synchronous native work runs on a thread-pool thread
for async APIs; cancellation can prevent a queued operation but does not interrupt
one already running. Connection calls while a transaction is active join it. Parameter getters and
enumerators run before taking the connection lock. If the transaction identity
changes during preparation (including auto-commit to an explicit transaction),
the pending query throws `InvalidOperationException` instead of changing scope.

Namespaces are `neo4j` and `system`; `system_ontologies` is available only when
`RELATE_PREVIEW=ontology` was set before opening the database. A statement's
`USE` target is reported by the result and does not change the session default.

## Parameters and input validation

Accept anonymous objects, readable public POCO/record properties, string-keyed
`IDictionary` / `IReadOnlyDictionary` and F# maps. Null means no parameters.
Nested lists and maps use the typed C builder with a maximum nesting depth of
64. Unsupported values, graph entities, invalid map keys and cycles exceeding
that depth fail before executing the statement. Reflection-based POCO mapping
requires preserved properties; trimmed/NativeAOT applications are not validated.

| Managed input | Native value |
| --- | --- |
| null / `DBNull`, bool | NULL, BOOL |
| Signed integer types, byte/ushort/uint, ulong ≤ Int64.MaxValue | INT (64-bit signed) |
| float/double/decimal | FLOAT; decimal converts to double and may lose precision |
| string/char, Guid, enum | STRING; Guid uses `D`, enum uses its name |
| `DateOnly`, `TimeOnly` | DATE, LOCAL TIME |
| UTC `DateTime`, `DateTimeOffset` | DATETIME with offset |
| Local/unspecified `DateTime` | LOCAL DATETIME |
| `CypherZonedDateTime`, `CypherOffsetTime` | DATETIME with IANA zone, TIME with offset |
| `TimeSpan`, `CypherDuration` | DURATION; TimeSpan uses seconds and nanoseconds |
| `CypherPoint`, `CypherVector` | POINT, typed VECTOR |
| IEnumerable / byte array, dictionary / object | LIST, MAP |

Locations, Cypher text, namespaces, parameter/map keys and zone IDs are C strings
and reject embedded NUL with `ArgumentException`. Length-delimited string
parameter values preserve embedded NUL and UTF-8. Always bind user values as
parameters instead of concatenating Cypher.

## Results and value types

`QueryResult` implements `IReadOnlyList<RelateRecord>` (`Count`, indexer,
enumeration). `Columns` preserves column order. `Scalar<T>()` reads the first
cell and rejects an empty result. Results own managed copies and survive
connection/database disposal; their memory grows with the final answer size.
Use `LIMIT` for bounded answers.

`RelateRecord` is an `IReadOnlyDictionary<string, object?>` with integer and
string indexers, `Keys`, `Values`, `Count`, `ContainsKey`, `TryGetValue`,
enumeration and `Get<T>(index/name)`. Duplicate output names cannot occur (the
engine rejects them at planning), so a duplicate column is rejected as contract
drift rather than silently resolved; index access retains every column. Missing
names throw `KeyNotFoundException`.
Typed conversions check numeric range, support arrays/lists/dictionaries,
Guid/enum parsing, duration-to-TimeSpan and vector array copies. Null cannot be
converted to a non-nullable value type; incompatible conversions throw.

| Native kind | Managed result / members |
| --- | --- |
| NULL, BOOL, INT, FLOAT, STRING | null, bool, long, double (including NaN/±Infinity), string |
| LIST, MAP | Read-only lists and string-keyed dictionaries of managed values |
| NODE | `Node`: `ElementId`, deprecated `Id`, `Labels`, `Properties`, property indexer, `HasLabel`, `Get<T>` |
| RELATIONSHIP | `Relationship`: `ElementId`, deprecated `Id`, `Type`, endpoint element/numeric IDs, `Properties`, property indexer, `Get<T>` |
| PATH | `GraphPath`: `Nodes`, `Relationships`, `Start`, `End`, `Length`; constructor requires one more node than relationship |
| DATE / LOCAL TIME / LOCAL DATETIME | `DateOnly` / `TimeOnly` / unspecified `DateTime` |
| TIME | `CypherOffsetTime(Time, Offset)` |
| DATETIME | `DateTimeOffset`, or `CypherZonedDateTime(DateTimeOffset, ZoneId)` with `ToDateTimeOffset` / implicit conversion |
| DURATION | `CypherDuration(Months, Days, Seconds, Nanoseconds)`; `FromTimeSpan`, `ToTimeSpan` |
| POINT | `CypherPoint(Srid, X, Y, Z?)`; `Dimension`, Cartesian/Wgs84 2D/3D factories and SRID constants |
| VECTOR | `CypherVector`: element type/name, dimension, integer-family flag; factories from singles/doubles/int32s/int64s; array-copy conversions |
| Future unknown kind | `UnsupportedValue(KindCode)` |

Graph entity constructors accept caller-provided collections; they do not clone
those inputs. Native-decoded collections are read-only. Node/relationship
equality uses element ID, so do not compare identities across different stores.
Vector equality compares element type and coordinates. `VectorElementType`
covers FLOAT32/FLOAT64 and INTEGER8/16/32/64. Integer arrays cannot be extracted
from a floating vector. Graph path relationships retain stored orientation.

BCL date/time mappings permit years 1–9999 and truncate sub-100 ns precision.
DATETIME offsets must fit `DateTimeOffset` (whole minutes, ±14 hours).
Out-of-range values throw. Duration components retain nanoseconds;
`ToTimeSpan` rejects nonzero months, treats a day as 24 hours, truncates sub-tick
precision and checks overflow. Public `ToString` methods are diagnostics, not
query serialization.

## Metadata, bookmarks and errors

`QueryResult.Namespace` is the resolved statement namespace. `QueryType` maps
`r/w/rw/s` to Read/Write/ReadWrite/Schema; the ABI guarantees exactly those
codes on a good result, so an unrecognized code is contract drift and throws a
`FormatException`. `Counters` contains NodesCreated/Deleted, RelationshipsCreated/Deleted,
PropertiesSet, LabelsAdded/Removed, IndexesAdded/Removed,
ConstraintsAdded/Removed and `ContainsUpdates`; `QueryCounters.Empty` is zero.
`Notifications` contain Code, GqlStatus, Severity, Category, Title, Description,
Remedy and nullable OperatorId. `PlanJson` is the optional `relate.plan/v1` JSON
plan. `Timings` contains Elapsed, Parse, Plan and Execute `TimeSpan` values.

`Bookmark` contains `Value` (`relate:<n>`) and nonnegative Int64 `Sequence`.
`Parse` throws on invalid/out-of-range input; `TryParse` returns false.
Equality, ordering, comparison operators and hashing use sequence numbers.
Result bookmarks are present for auto-commit mutations; explicit transactions
return their bookmark from commit. Null means no commit sequence was produced.

`RelateException` extends Exception and has default, message, message+inner,
and full-detail constructors. It carries `Code`, `GqlStatus`, `Stage`
(Unknown/Compile/Runtime/Internal) and `IsRetryable`. Both query errors and
native out-parameter errors preserve the structured engine details. Native
strings/results/builders are released even if decoding throws. Invalid ABI
collection lengths or missing handles/keys throw rather than fabricate values.
Missing libraries throw `DllNotFoundException`; incompatible binaries can throw
`BadImageFormatException`, and a mismatched ABI major throws `NotSupportedException`.

## Runnable examples

Run `dotnet run --project docs/api/dotnet/examples` from the repository root
after building `relate-ffi`. [Program.cs](examples/Program.cs) asserts typed
parameters, graph values, metadata, namespaces, transactions, errors, async
queries and result lifetime. The adopted [three-language tests](../../../clients/dotnet/tests)
exercise the remaining value/conversion factories and lifecycle APIs.
