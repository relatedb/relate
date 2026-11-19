---
type: adr
area: infra
date: 2026-09-19
deciders: CypherLite maintainers
name: 0067-release-versioning-and-process
status: accepted
tags:
  - release
title: |-
  Release and versioning policy: manual dispatch, tag-after-build, lockstep Rust/Python versions
---

# 0067. Release and versioning policy: manual dispatch, tag-after-build, lockstep Rust/Python versions

## Context

CypherLite has never had a deliberate release process. The repository
inherited a tag-push-triggered release workflow (`on: push: tags: ["v*"]`):
any pushed `v*` tag kicked off the release pipeline, so the tag was the
*input* to the run. That ordering has two weaknesses for a small team: any tag
— accidental or not — triggers a release, and a tag can be published before
anyone has confirmed the artifacts behind it actually build.

Linear issue **LABS-52** ("Release script") asks for a real release ceremony
and fixes the versioning baseline: the first stable release is **0.4.0**, and
pre-release versions (the tree currently carries `0.3.0-alpha`) are skipped
going forward. A self-updating CLI tool's release workflow
(manual-dispatch `.github/workflows/release.yml` shape) was used as the
reference model for the manual-dispatch shape, though not for its tag
ordering (see Alternatives considered).

Versioning today is split across surfaces: the Rust workspace version lives in
the root `Cargo.toml` under `[workspace.package]` and is inherited by every
crate; the Python package version lives in `pyproject.toml` with fallbacks in
`__init__.py`; the JavaScript client (`clients/javascript/package.json`) and
Java client (`clients/java/pom.xml`) each carry their own `0.1.0` version.
Deciding which of these the release workflow bumps — and which it leaves
alone — is part of this decision.

A practical constraint shapes the workflow design: `main` has **no branch
protection** (verified via the GitHub API), so the workflow's `GITHUB_TOKEN`
can push a version-bump commit directly to `main` without a pull request.

## Decision

Adopt the following release and versioning policy.

1. **Manual trigger only.** Releases are triggered exclusively by
   `workflow_dispatch` on the Release workflow, with a `bump` choice input
   (`patch` / `minor` / `major`, default `patch`). The previous tag-push
   trigger (`on: push: tags: ["v*"]`) is removed. A release is a deliberate,
   human-gated act.
2. **Tag-after-build.** The git tag `vX.Y.Z` is created and pushed by the
   workflow only *after* the cli, ffi, and python build jobs all succeed. A
   failed build leaves no dangling tag: the tag is an output of a successful
   run, never an input to it.
3. **Lockstep Rust + Python versioning.** The Rust workspace version
   (`[workspace.package]` in the root `Cargo.toml`, inherited by all crates)
   and the Python package version (`pyproject.toml` plus the `__init__.py`
   fallbacks) are versioned in lockstep and bumped together by the release
   workflow. The first stable release is **0.4.0**; pre-release versions such
   as the current `0.3.0-alpha` are skipped going forward.
4. **Independent JS and Java client versions.** The JavaScript client
   (`clients/javascript/package.json`, `0.1.0`) and the Java client
   (`clients/java/pom.xml`, `0.1.0`) are versioned independently and are **not**
   bumped by the release workflow.
5. **Automated notes and changelog.** Release notes are GitHub auto-generated
   (`generate_release_notes: true`). CHANGELOG.md rotation is automated in the
   version-bump commit: the `Unreleased` content moves under a new
   `## [X.Y.Z] — <date>` section. Because `main` is not branch-protected, the
   workflow pushes this bump commit directly.

## Alternatives considered

- **Keep tag-push-triggered releases (the status quo ante)** — rejected: the
  team wants human-gated releases where the tag is an *output* of the run,
  not the input. Under tag-push, any pushed `v*` tag — including a mistaken
  one — fires the pipeline, and the tag exists publicly before the artifacts
  behind it have been proven to build.
- **Tag-before-builds ordering (the reference tooling's ordering)** — rejected:
  the reference workflow creates the tag first and builds afterwards, so a
  failed build leaves a published tag with no release behind it, and cleaning
  that up requires either deleting a pushed tag or burning a version number.
  Tag-after-build makes a failed run leave no trace.
- **Version bump via pull request** — rejected: `main` is not
  branch-protected, so a PR round-trip adds a manual step to an already
  manual ceremony without buying any additional review guarantee beyond what
  the human-gated dispatch already provides.
- **Bump all client versions in lockstep (JS and Java included)** — rejected:
  the JS and Java clients move on their own cadence and their `0.1.0`
  versions do not track the Rust/Python line; forcing them into the release
  workflow would couple surfaces that ship independently.

## Consequences

- Releases require a deliberate `workflow_dispatch` invocation; no tag push,
  accidental or otherwise, can start one.
- A failed release run is invisible from the outside: no tag, no GitHub
  release, no half-published artifacts. Re-running after a fix is safe.
- Consumers can rely on the Rust crates and the Python package sharing one
  version number, starting at 0.4.0. The `0.3.0-alpha` line is abandoned,
  not continued.
- The JS and Java clients must be versioned and released by their own
  processes; this ADR does not define those, and their manifests stay at
  `0.1.0` until such a process exists.
- CHANGELOG.md grows a dated section per release automatically; the
  `Unreleased` section resumes empty after each bump commit.
- Because the bump commit is pushed directly to an unprotected `main`, the
  workflow depends on that protection state. If branch protection is later
  enabled on `main`, the workflow's push will fail and this decision must be
  revisited (new ADR) — likely moving the bump commit to a PR or using a
  bypass-capable token.
- Changing any of these points (trigger shape, tag ordering, version
  coupling, notes/changelog automation) requires a new ADR referencing this
  one.
