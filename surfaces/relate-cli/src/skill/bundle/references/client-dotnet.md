# .NET client: Relate.Net

## Install

Source integration under `clients/dotnet`; .NET 8+ runtime, .NET 10 SDK for the
three-language tests. Build `cargo build -p relate-ffi`, then
`dotnet test clients/dotnet/Relate.Net.sln`. Add a project reference to
`clients/dotnet/src/Relate.Net/Relate.Net.csproj`. No registry publication is
configured by this integration. Local packages contain one native RID.

## Minimal usage

```csharp
using Relate.Net;
using var db = RelateDatabase.OpenInMemory();
using var conn = db.Connect();
var answer = conn.Query("RETURN $x + 1 AS n", new { x = 41 });
Console.WriteLine(answer.Scalar<long>()); // 42
```

## Gotchas

- Build the native library from the same checkout. Override its path with
  `-p:RelateNativeLibraryPath=/absolute/path/to/library`; the default is the
  host library in `target/debug`. C ABI major 2 is checked on load.
- Dispose databases, connections and transactions. Uncommitted transactions
  roll back. A connection serializes calls; use one per parallel worker.
- Results are materialized; bound large answers with Cypher `LIMIT`.
  `Namespace` and `QueryType` describe the statement, not the session default.
- Bind values as parameters. Embedded NUL is rejected in C-string inputs,
  including query text and parameter keys, but preserved in string values.
- Async schedules synchronous native work. Cancellation stops queued calls,
  not a statement already executing. BCL temporals have 100 ns precision and
  years 1–9999; durations retain their original components.
- Inspect `RelateException.Code`, `GqlStatus`, `Stage`, `IsRetryable`.
  Commit can fail and must be included in retry handling.

## Spec

`docs/api/dotnet/spec.md`; runnable example in `docs/api/dotnet/examples`.
The full user guide is `clients/dotnet/docs/UserGuide.md`.
