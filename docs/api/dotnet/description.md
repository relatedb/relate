# .NET client — description

`Relate.Net` embeds Relate in C#, F# and Visual Basic applications through the
same typed C ABI as the other native language clients. There is no server,
network driver, or alternate engine. Use it for local graph queries and durable
stores inside a managed application.

The API uses disposable databases, connections and transactions; parameter
objects become typed builder entries. Results expose managed records, graph
entities, temporals, vectors and points plus the resolved namespace, query type,
mutation counters, bookmark, notifications, plan and timings. Structured
exceptions retain engine error codes and retryability.

This adopts [Peter Villadsen’s client](../../../clients/dotnet/UPSTREAM.md)
and its three-language contract suite. Its public API remains; it is
Apache-2.0 licensed ([ADR-0625](../../adr/0625-adopt-the-dotnet-client.md)).
Native builds come from this checkout, and each release publishes the package to
nuget.org as `Relate.Net` — all five RIDs in one package, keyless, through a
trusted-publishing policy on the release pipeline ([ADR-0652](../../adr/0652-publish-the-dotnet-client-to-nuget-via-trusted-publishing.md)).
Install it with `dotnet add package Relate.Net`.

See [specification](spec.md), [user guide](../../../clients/dotnet/docs/UserGuide.md)
and the [executable example](examples/Program.cs). Build requirements and
platform-specific native paths are in the [client README](../../../clients/dotnet/README.md).
