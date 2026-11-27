---
type: adr
area: infra
claim: 530dc0e70e19be34d8f57e879a50dcab13e9661f
date: 2026-11-27
deciders: the maintainers
name: 0710-pypi-publishes-inline-in-the-release-workflow
status: proposed
tags:
  - ci
  - python
  - release
title: PyPI publishes inline in the release workflow
REFERENCES:
  - "[[adr/0560-crates-io-and-pypi-distribution]]"
  - "[[adr/0652-publish-the-dotnet-client-to-nuget-via-trusted-publishing]]"
  - "[[adr/0704-maven-central-publishes-with-every-release]]"
---

# 0710. PyPI publishes inline in the release workflow

## Context

[ADR-0560](./0560-crates-io-and-pypi-distribution.md) put every registry
publish in one workflow, `publish-registries.yml`, callable from
`release.yml` and dispatchable by hand. A chained (called-workflow) run's
OIDC token names the *top-level* workflow — `release.yml` — and each
registry decides whether that identity is acceptable. npm and crates.io
accept it: both registered `release.yml` as a trusted publisher, and the
v1.4.0 cut published all seven npm packages and the 17-crate closure
green through the call. PyPI structurally cannot: its Trusted Publishing
does not support reusable workflows at all ("not currently supported …
subject to breakage", in PyPI's own words), so no registration can ever
make the chained token acceptable. The v1.0.0, v1.3.0 and v1.4.0 cuts
each failed the chained PyPI leg and were recovered by a remembered
manual `publish-registries.yml` dispatch against the tag.

## Decision

**The PyPI publish is a first-class job in `release.yml`
(`publish-pypi`), not a leg of the `publish-registries.yml` call.**

Same steps as the call's PyPI leg, hoisted: download the release's
wheels and sdist, gate the count, verify each file against the release
run's build attestations, publish through trusted publishing
(`id-token: write`, no token anywhere). It gates on the same
`!offline && registries` opt-in the call keeps, and runs in the
`release` environment so the main-only branch policy binds it. The PyPI
trusted publisher for `relate-db` names `release.yml`;
`publish-registries.yml` keeps its own registration and its PyPI leg as
the direct-dispatch recovery and backfill arm (the v1.3.0 and v1.4.0
recoveries proved that arm), with the leg off by default on the
`workflow_call` form so no chained run attempts it again.

## Alternatives considered

- **Keep the leg in the call; recover by dispatch.** Rejected: every
  cut with `registries=true` fails a leg by construction and waits on a
  remembered manual recovery — three consecutive cuts proved the
  pattern.
- **Register `release.yml` on PyPI and keep the leg chained.**
  Impossible: PyPI rejects the reusable-workflow shape itself, not the
  identity; that registration cannot exist.
- **Flip `registries` on by default, now that every OIDC leg can publish
  from a cut.** Deferred with [ADR-0704](./0704-maven-central-publishes-with-every-release.md)'s
  reasoning: the default is its own decision; this one only makes the
  opt-in publish everything it names.

## Consequences

- A release cut with `registries=true` publishes `relate-db` to PyPI
  from its own run, green by construction; the run's log is the publish
  record, and the first cut after the `release.yml` registration lands
  proves it end to end.
- Until that registration lands (one-time, outside this repository),
  the inline job fails closed at the token exchange, exactly like an
  unregistered registry.
- The call's chained legs are npm, crates.io and Chocolatey; the
  `registries` input description, `docs/PUBLISHING.md` and
  `RELEASING.md` name the new shape.
- TestPyPI dry runs (`pypi-target=testpypi`) stay on the dispatch arm;
  the inline job has no dry-run form.
- The PyPI leg's steps now exist in two workflows — the one duplication
  the constraint forces: the shared form (a called workflow) is exactly
  what PyPI rejects.
