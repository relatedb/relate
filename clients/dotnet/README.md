# Relate .NET client

An embedded .NET 8+ client over Relate's typed C ABI, usable from C#, F# and
Visual Basic. Adapted from [Peter Villadsen’s implementation](UPSTREAM.md),
offered in [issue #410](https://github.com/relatedb/relate/issues/410).
The contributed API and tests are retained. The client is [Apache-2.0 licensed](LICENSE)
([ADR-0625](../../docs/adr/0625-adopt-the-dotnet-client.md)); the upstream MIT notice
is preserved in [third-party notices](THIRD-PARTY-NOTICES.md).

## Build and test

From the repository root, with Rust 1.88 and the .NET 10 SDK plus .NET 8 runtime
(the F# tests use the SDK's newer compiler):

```sh
cargo build -p relate-ffi
dotnet restore clients/dotnet/Relate.Net.sln --locked-mode
dotnet test clients/dotnet/Relate.Net.sln --no-restore
dotnet run --project docs/api/dotnet/examples
```

Managed dependencies restore from NuGet.org into an isolated `.packages` cache
under this client. Source mapping excludes SDK-bundled copies so the committed
package hashes remain consistent across SDK installations.

The build uses `target/debug/relate_ffi.dll` (Windows),
`target/debug/librelate_ffi.dylib` (macOS), or `target/debug/librelate_ffi.so`
(Linux), copying the library beside managed output. It never downloads a release.
Supported native runtime identifiers: `win-x64`, `linux-x64`, `linux-arm64`,
`osx-x64`, `osx-arm64`. CI runs Linux x64, Windows x64 and macOS ARM64.
The runtime checks C ABI major version 2 when first loading the library.

To use another build directory or release profile, pass the absolute native path:

```sh
dotnet test clients/dotnet/Relate.Net.sln \
  -p:RelateNativeLibraryPath=/absolute/path/librelate_ffi.dylib
```

`RelateNativeRuntimeIdentifier` defaults to the SDK host RID. Override it when
packing a native library for a different host. `dotnet pack` produces a **local,
single-RID package**, version `1.1.1-local`; it is not a multi-platform release:

```sh
dotnet pack clients/dotnet/src/Relate.Net --output clients/dotnet/artifacts
```

This integration does not publish to NuGet or change ownership of the existing
community `Relate.Net` package. To consume source, add a project reference to
`clients/dotnet/src/Relate.Net/Relate.Net.csproj`; the native asset is copied
transitively. A packaged application needs the matching native RID asset.

## Minimal usage

```csharp
using Relate.Net;

using var db = RelateDatabase.OpenInMemory();
using var conn = db.Connect();
conn.Query("CREATE (:Person {name: $name})", new { name = "Ada" });
var result = conn.Query("MATCH (p:Person) RETURN p.name AS name");
Console.WriteLine(result[0].Get<string>("name")); // Ada
```

Use `RelateDatabase.Open(path)` for persistence, `OpenReadOnly(path)` for a reader,
and `BeginTransaction()` / `Commit()` for explicit transactions. Disposing an
uncommitted transaction rolls it back. Disposing the database closes its
connections. A connection serializes operations; use separate connections for
parallel work.

Results are fully materialized and survive connection disposal. Bound large
answers with Cypher `LIMIT`. Async methods schedule synchronous native calls;
cancellation can prevent a queued operation but cannot interrupt a running one.
Temporal BCL mappings have 100 ns precision and years 1–9999. C-string arguments
reject embedded NUL; string parameter **values** preserve it.

See the [API specification](../../docs/api/dotnet/spec.md),
[user guide](docs/UserGuide.md), and [runnable example](../../docs/api/dotnet/examples).
See the [adoption decision](../../docs/adr/0625-adopt-the-dotnet-client.md) for scope and design rationale.
