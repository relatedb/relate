---
type: adr
area: infra
claim: 0b368048db240d8303bece3aaf8df69b82cf224f
date: 2026-11-27
deciders: the maintainers
name: 0704-maven-central-publishes-with-every-release
status: proposed
tags:
  - release
  - ci
  - maven
title: The Java client publishes with every release
REFERENCES:
  - "[[adr/0630-maven-central-automatic-publish]]"
  - "[[adr/0652-publish-the-dotnet-client-to-nuget-via-trusted-publishing]]"
---

# 0704. The Java client publishes with every release

## Context

[ADR-0630](./0630-maven-central-automatic-publish.md) made the Maven
Central publish green and self-contained — credentials forwarded, plugin
pinned, `autoPublish=true` — but kept it behind the same `registries`
opt-in as the OIDC registries: "one opt-in for every registry … an
unchecked dispatch publishes nothing anywhere, the Java client included."
The uniformity rationale never applied to Central (it authenticates a
static token, not the run's identity), and the cost has since shown up
twice:

1. **An ordinary release dispatch skips the Java client silently.** The
   v1.2.0 and v1.3.0 cuts both needed a remembered, manual
   `publish-registries.yml` dispatch afterwards to put the jars on
   Central; nothing in the release run says a step is outstanding.
2. **The recovery dispatches leave debris.** Dispatching against a tag
   whose version Central already carries fails at the publish goal with
   `already exists` — after creating a deployment record in the Portal.
   The v1.3.0 day left several dead 1.2.0 deployments in the Portal's
   list from exactly these attempts.

## Decision

**The chained `publish-maven-central` job in `release.yml` gates on
`!offline` alone: every non-offline release publishes the Java client to
Maven Central, with no opt-in.**

The `registries` input keeps its gate over the OIDC chain call
(`publish-registries`: npm, PyPI, crates.io) unchanged — those registries
reject a chained run's identity until `release.yml` is registered on each
([ADR-0630](./0630-maven-central-automatic-publish.md) context,
docs/PUBLISHING.md). Its description no longer names the Maven jars. The
direct dispatch of `publish-registries.yml` with `maven=true` stays the
recovery arm, and `maven-dry-run` stays the rehearsal against `main`;
neither is part of an ordinary release anymore.

Everything else keeps [ADR-0630](./0630-maven-central-automatic-publish.md)'s
shape: the four credentials ride the call, the job runs in the `release`
environment with its branch policy, the jars come from the release's own
attested FFI assets, and the version comes from the tag's tree — the job
checks out `registry-owner.outputs.sha`, so the published version is the
tag's pom, never the dispatching branch's.

## Alternatives considered

- **Keep the shared opt-in ([ADR-0630](./0630-maven-central-automatic-publish.md)
  §2).** Rejected: uniformity with registries that cannot chain costs a
  silent skip on every ordinary release, and the manual recovery it
  forces is what litters the Portal with dead deployments.
- **Default `registries=true` for the whole chain.** Rejected for now:
  the OIDC legs fail on the chained identity until a per-registry
  registration happens outside this repository; flipping the default
  before that just moves the red. A later ADR flips it when the
  registrations land.

## Consequences

- A non-offline release always carries the Java client; forgetting is no
  longer possible, and the release run's own log is the publish record.
- `offline=true` still skips it — the escape for a GitHub-only release.
- The OIDC chain (npm, PyPI, crates.io) and NuGet
  ([ADR-0652](./0652-publish-the-dotnet-client-to-nuget-via-trusted-publishing.md))
  remain manual dispatches until their identity questions are settled;
  this ADR narrows the manual set, it does not abolish it.
- The Portal deployments view gains one validated deployment per release
  and loses the dead ones the recovery path used to mint.
- docs/PUBLISHING.md's Maven Central section moves the automatic publish
  into the release description and keeps the dispatch as recovery only.
