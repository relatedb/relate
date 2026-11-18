# Upstream attribution

This client is adapted from [Peter Villadsen’s relate.NET](https://github.com/peter-villadsen/relate.NET),
commit `1452ad288971e6af4f30d369deb2bcb894471d1f`, offered for adoption in
[relatedb/relate#410](https://github.com/relatedb/relate/issues/410).

The original implementation, public API, source-generated P/Invoke bindings,
value mapping, user guide and C#, F# and Visual Basic tests are retained and
adapted for this repository. Copyright (c) 2026 Peter Villadsen. This client is
licensed Apache-2.0 with the repository ([ADR-0625](../docs/adr/0625-adopt-the-dotnet-client.md)),
as the contributor offered; the upstream MIT notice is preserved in
[third-party notices](THIRD-PARTY-NOTICES.md).

Integration changes replace release downloads with the current checkout's native
build, support the repository's host platforms, harden interop/lifecycle edges,
and add repository documentation and CI coverage. No upstream publishing workflow
or NuGet credentials are imported. Package ownership and publication remain
separate from this source adoption.
