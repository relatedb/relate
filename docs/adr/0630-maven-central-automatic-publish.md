---
type: adr
area: infra
claim: 75e819c25afd893838cb80322c47056677124e49
date: 2026-11-20
deciders: the maintainers
name: 0630-maven-central-automatic-publish
status: proposed
tags:
  - release
  - ci
  - maven
  - security
title: Maven Central publishes automatically from the release run
REFERENCES:
  - "[[adr/0560-crates-io-and-pypi-distribution]]"
  - "[[adr/0490-java-bundled-natives]]"
  - "[[adr/0125-release-notes-amendments]]"
---

# 0630. Maven Central publishes automatically from the release run

## Context

The Java client publishes to Maven Central through the
`central-publish` profile of `clients/java/pom.xml`, driven by the
`maven-central` job in `publish-registries.yml`: the fat jar plus the
five platform classifier jars ([ADR-0490](./0490-java-bundled-natives.md)),
signed with a GPG key and uploaded with a Central Portal token. The
registry publish machinery ([ADR-0560](./0560-crates-io-and-pypi-distribution.md))
is direct-dispatch by default — the OIDC registries reject the chained
call's identity — and the v1.2.0 cut added a chained
`publish-maven-central` job to `release.yml` so the Java client would
ship with every release. Its first live run failed twice over, in two
unrelated places:

1. **The chained job imported an empty signing key** (run 44705331863).
   The call forwarded no secrets at all: a called workflow's `secrets`
   context carries only what its caller explicitly passes — ambient
   repository secrets included (the rule the release-notes API key
   already follows, [ADR-0125](./0125-release-notes-amendments.md)
   amendment 3). A `uses` job cannot bind an `environment` either, so
   environment-scoped forwarding is unavailable too. The four
   credentials have been repository-level secrets since owner setup;
   the v1.2.0 call simply handed the called workflow nothing.
2. **The recovery dispatch uploaded and then went red** (run 44711696712).
   The Central Portal API began returning a `warnings` field in the
   deployment-status response; the pinned
   `central-publishing-maven-plugin` 0.7.0 model does not know the field
   and its strict deserializer fails the build while polling validation,
   after the jars are already uploaded.

The profile also sets `autoPublish=false`, leaving a manual Portal press
as the release's last step — a deliberate choice for the first
publishes that no longer earns its ceremony.

## Decision

**The Java client publishes automatically, green, from a release
dispatched with `registries=true` — and from nothing else.**

1. **The chained call forwards the four credentials explicitly.**
   `MAVEN_GPG_KEY`, `MAVEN_GPG_PASSPHRASE`, `MAVEN_CENTRAL_USERNAME` and
   `MAVEN_CENTRAL_TOKEN` are repository-level secrets — they have been
   since owner setup; this ADR moves nothing. `release.yml`'s
   `publish-maven-central` job forwards them to the called workflow;
   `publish-registries.yml` declares the four in its
   `workflow_call.secrets` — the declare-and-pass pattern
   [ADR-0125](./0125-release-notes-amendments.md) amendment 3 set for the
   release-notes API key. A direct dispatch reads them ambiently. The
   `maven-central` job keeps its `environment: release` binding for the
   main-only branch policy. The job refuses empty credentials with a
   named error before the GPG import — a `registries=true` release
   dispatched without them fails loud in the publish leg, never
   cryptically.
2. **One opt-in for every registry.** The chained job gates on
   `!offline && inputs.registries`, exactly like the OIDC chain call: an
   unchecked dispatch publishes nothing anywhere, the Java client
   included. A `registries=true` release deploys the jars exactly once —
   the OIDC call leaves `maven` at its false default — and needs no
   registry-side registration, because Central authenticates the static
   token, not the run's identity.
3. **The plugin is pinned at 0.12.0**, whose deployment model carries
   the `warnings` field (0.7.0's does not — verified against both jars
   on Central).
4. **`autoPublish=true`.** Central's validation is the release gate: a
   deployment that validates goes live on its own; a failed validation
   fails the job. The human review moves from "press Publish in the
   Portal" to "read the publish leg's log" — and a red leg still holds
   nothing back, because the release itself is already published by the
   time the registry legs run. The Portal press was never an
   authorization boundary: the user token can publish a validated
   deployment through the Publisher API itself
   (`POST /api/v1/publisher/deployment/<id>`, or an `AUTOMATIC`
   upload), so `autoPublish=false` gates only this repository's own
   lane, never a stolen token's reach.
5. **The dispatch remains the recovery arm.** Re-dispatching
   `publish-registries.yml` against the tag with `maven` on and the
   other registries off publishes the jars alone;
   `maven-dry-run=true` still rehearses the profile against `main`
   without uploading.

## Consequences

- The repository-level placement **predates this ADR** — it was the
  standing configuration at owner setup, not a widening this ADR
  accepts. The exposure it carries is therefore unchanged by this
  decision: repository secrets are readable by any workflow in this
  repository on any branch, including `pull_request` runs from
  in-repo branches; only fork pull requests never see them. The
  compensations are the rotation runbook (docs/PUBLISHING.md, Maven
  Central) and the `maven-central` job's main-only branch policy on
  its dispatch path. The signing key keeps its offline backup; a GPG
  key cannot be replaced for artifacts already published.
- `maven-dry-run` stops before the `deploy` goal, so it validates the
  profile (enforcer, packaging, sources, javadoc, signatures) but not
  the publish goal itself — the first `registries=true` release is the
  plugin bump's live verification.
- Published versions are immutable; `autoPublish=true` makes the Portal
  deployments view a record rather than a queue.

## Later references

- [ADR-0704](./0704-maven-central-publishes-with-every-release.md) lifts
  this ADR's §2 "one opt-in for every registry": the Java publish now
  gates on `!offline` alone, so every ordinary release publishes the
  jars. The OIDC chain keeps the `registries` opt-in.
