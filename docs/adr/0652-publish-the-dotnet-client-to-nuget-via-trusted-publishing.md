---
type: adr
area: infra
claim: d6246121aa087d961510d7223aa0bbdae38ef3d8
date: 2026-11-21
deciders: the maintainers
name: 0652-publish-the-dotnet-client-to-nuget-via-trusted-publishing
status: proposed
tags:
  - clients
  - dotnet
  - release
title: The .NET client publishes to nuget.org through a trusted-publishing policy
REFERENCES:
  - "[[adr/0560-crates-io-and-pypi-distribution]]"
  - "[[adr/0625-adopt-the-dotnet-client]]"
  - "[[adr/0630-maven-central-automatic-publish]]"
---

# ADR-0652: The .NET client publishes to nuget.org through a trusted-publishing policy

## Status

Proposed — lands with its implementation.

## Context

The .NET client ([ADR-0625](0652-adopt-the-dotnet-client.md)) is adopted from an
upstream client that already owns the `Relate.Net` package ID on nuget.org: a
`1.0.0-preview.1` prerelease carries that author's own wrapper and a single
platform's native library. This repository now shares ownership of the ID, and a
release has to publish official versions to it.

Every other registry this project publishes to rides OIDC trusted publishing
([ADR-0560](0560-crates-io-and-pypi-distribution.md) for PyPI and crates.io, and
the npm packages): the repository holds no long-lived registry credential, the
publishing job's identity is exchanged for a short-lived one at publish time, and
the registry side registers *which* repository, workflow file, and environment may
do the exchange. Maven Central is the one exception
([ADR-0630](0630-maven-central-automatic-publish.md)), because Central
authenticates a static token rather than the run's identity.

nuget.org supports the same trusted-publishing shape: a policy naming the
repository owner, the repository, a workflow file, and optionally a GitHub
Actions environment is validated against the run's OIDC token, which the registry
exchanges for a one-hour API key.

Two facts about the client shape the decision. Its native library is the C ABI
(`relate-ffi`), built per platform, and the client declares five supported RIDs
(`win-x64`, `linux-x64`, `linux-arm64`, `osx-x64`, `osx-arm64`): a package that
carries one RID cannot serve a release, since the RID a consumer needs follows
their machine, not the packer's. And the published version must equal the release
tag, as it does for every other client package — a package named for a stale
project version would publish a version nobody asked for, and nuget.org's version
list is append-only.

## Decision

**1. One fat package per release, carrying every supported RID.** The release
builds it from that release's own FFI artifacts — the same bytes the other native
clients stage — so the package can never ship a native library the release did
not build. It rides the GitHub release as an attested asset, because publishing
reads only release assets: a package attached after the fact cannot ride the
trusted publisher, exactly as for the wheels and tarballs.

**2. The client's targets file carries the RID vocabulary once, and gains a
release staging mode.** Each supported RID and the library file name it ships in
are one table; the local single-RID pack path is unchanged for development and
the client's own CI. The release mode, `RelateNativeDistDir`, packs
`<rid>/<library>` for every RID in that table into one package and **refuses a
missing RID** rather than shipping a short package.

**3. Publishing is a leg of the registry publisher, not of the release.**
`publish-registries.yml` gains a `nuget` job: the `release` environment,
`id-token: write`, the login action exchanging the run's OIDC token for a
short-lived key, then `dotnet nuget push` of the package and its symbols package
against nuget.org. It verifies the package's build provenance first, like every
other registry leg.

**4. The leg publishes by direct dispatch only.** The registered policy names
workflow `publish-registries.yml` and environment `release`; a chained call from
`release.yml` presents that workflow's own identity instead
([ADR-0560](0560-crates-io-and-pypi-distribution.md) recorded the same mismatch
for PyPI). The leg's input therefore defaults off on the called path, and the
runbook publishes NuGet by dispatch against the tag.

**5. The published version equals the release tag.** The client project's
`<Version>` joins the release sweep's version surfaces, and the release job's
package check refuses a package whose name is not the release version — the
failure lands in the release run, not on the registry.

**6. The policy's environment field is part of the binding.** Registering
`release` on the nuget.org policy makes a token minted outside that environment
unusable even if the repository's branch rule is ever weakened; the environment's
deployment branch rule (main only) is the repository-side half, as for the other
registries.

## Consequences

- No nuget.org credential exists in the repository: nothing to store, rotate, or
  leak. The one new secret is the policy owner's profile name, which is an
  identifier rather than a credential.
- A release publishes NuGet in a second, direct dispatch — the same operational
  shape PyPI already has. A failed leg is recovered by re-dispatching the same
  tag with the leg enabled; nuget.org skips a version it already holds.
- The upstream `1.0.0-preview.1` stays in the version history, and the package
  page keeps the upstream author and project URL until this repository publishes
  its own metadata. Official versions continue the ID; the two ownerships are
  visible on nuget.org.
- The release gains one packaging job and the release payload one asset; a broken
  pack blocks the tag like every other artifact.
- The client's own CI keeps packing the host's single RID, so a change to the RID
  table is exercised locally without the release path.
- The `RelateNativeDistDir` mode is release-only and pack-only: a release pack's
  build output is never used, the package is the artifact.
