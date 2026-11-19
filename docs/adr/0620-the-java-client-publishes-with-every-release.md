---
type: adr
area: infra
claim: f4ff7f7522b2c6dfc00cf2ae9174d1bba7d67aec
name: 0620-the-java-client-publishes-with-every-release
status: proposed
tags:
  - release
title: The Java client publishes with every release
REFERENCES:
  - "[[adr/0591-java-client-maven-central-coordinates]]"
---

# 0620. The Java client publishes with every release

## Context

[ADR-0591](./0591-java-client-maven-central-coordinates.md) fixed the Java client's coordinates and recorded publication as a
manual dispatch of the registries workflow: Maven Central is the one
registry without trusted publishing, so it authenticates with a project
key and a Portal token held as environment secrets rather than with the
run's identity.

That made the Java client the only shipped artifact a release could omit.
One did: the registries call was chained behind an off-by-default opt-in,
and the Maven job's own gate then failed on a pom the publish profile had
never been exercised against. The profile is inactive everywhere CI runs,
so its rules were first parsed at publish time.

## Decision

- Maven Central publishes as part of every release. `release.yml` chains a
  Maven-only call to the registries workflow, gated on the release not
  being offline, beside the OIDC registries' opt-in call. The two calls
  are disjoint — each registry's flag is set on exactly one — so a release
  that opts into the OIDC registries still deploys the jars once.
- The registries workflow gains a Maven dry run: the publish profile's
  enforcer, packaging, sources, javadoc and GPG signing run against `main`
  with the tag's natives, stopping short of the upload. The profile is
  otherwise never exercised, so this is the rehearsal that catches a
  configuration the pinned plugins reject.
- Auto-release stays off. A deploy lands in the Central Portal's
  Deployments list and the release step remains a human act.

## Alternatives considered

- **Keep publication a manual dispatch, as [ADR-0591](./0591-java-client-maven-central-coordinates.md) records.** Rejected:
  the jars are a shipped artifact like the wheels and the tarballs, and a
  release that silently omits one is not a state worth preserving.
- **Publish Maven on the existing opt-in registries call.** Rejected: that
  call is off by default because the OIDC registries reject the release
  run's identity, so Maven would inherit an opt-in it does not need and
  stay omittable.
- **Rehearse by dispatching against the release tag.** Rejected: the
  publish job checks out the tag's tree, so a rehearsal against an
  already-cut tag re-tests the configuration that tag shipped, not the one
  the next release will carry.

## Consequences

- A release cannot ship without the Java client; the jars reach the Portal
  with no separate dispatch.
- A broken publish profile fails the dry run rather than the release.
- The dry run signs with the real key and uploads nothing, so the Portal
  credentials and the namespace stay unexercised until the first real
  deploy.
- The chained job authenticates with the `release` environment's secrets;
  a release run without them fails its Maven job.
