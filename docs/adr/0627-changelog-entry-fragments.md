---
type: adr
area: infra
claim: dc741ae81528030b692a57b6ca168625713836c8
date: 2026-11-19
deciders: the maintainers
name: 0627-changelog-entry-fragments
status: proposed
tags:
  - changelog
  - ci
  - release
title: Changelog entries arrive as .changes/ fragments
REFERENCES:
  - "[[adr/0580-post-release-workspace-versioning]]"
  - "[[adr/0618-release-notes-corpus-and-area-groups]]"
---

# 0627. Changelog entries arrive as .changes/ fragments

## Context

[ADR-0618](./0618-release-notes-corpus-and-area-groups.md) §4 pinned changelog entry discipline to direct edits under
`## [Unreleased]`: every pull request that carries a user-facing change
edits the same few lines of CHANGELOG.md, so every changelog-carrying
pull request conflicts with every other at rebase time. This repository
runs squash-rebase pull requests with parallel agent authors — the
conflict tax is paid on every fix round, and re-applying an entry
around conflicts is exactly how entries land under released sections,
the failure the placement guard exists for.

## Decision

**Entries queue as one fragment file per pull request under `.changes/`,
and the release cut assembles them.**

1. **Fragment shape.** One markdown file per pull request, named
   `<PR number>-<slug>.md`; the number makes cross-pull-request file
   collisions impossible and orders the collected entries. Content is
   one or more `###` section blocks — the Keep a Changelog sections,
   plus `Notes` for the maintainer record — whose bullets are the entry
   text verbatim. `scripts/changes_collect.py` is the one home of this
   grammar: `--check` validates the pool in CI, the collect mode runs
   at the cut.
2. **Collection at the cut.** The release sweep splices the fragments
   under `## [Unreleased]` — sections in Keep a Changelog order, entries
   in ascending pull-request order — then the existing rotation
   (unchanged) moves the section under the version heading, and the
   sweep deletes the consumed fragments in the same commit. The
   advance's scoped mode ([ADR-0580](./0580-post-release-workspace-versioning.md)) still skips the changelog entirely:
   fragments are version-independent and ride into the next cut.
3. **CHANGELOG.md is release-owned.** A pull request never edits it.
   The placement guard refuses any CHANGELOG.md diff; the
   `changelog-repair` label stays the sanctioned escape for correcting
   the published record. The presence warning fires when a kept
   *User-facing impact* section adds no fragment. Both checks keep
   their [ADR-0618](./0618-release-notes-corpus-and-area-groups.md) §4 shapes; only the entry channel moves.
4. **Migration.** The unreleased entries at adoption move into
   fragments named after the pull requests that landed the work, so the
   first cut under this ADR publishes them unchanged.

## Alternatives considered

- **Keep direct `[Unreleased]` edits** — rejected: the same-line
  conflict is the failure being fixed; nothing else removes it.
- **A section-directory layout (`added/`, `changed/`, …)** — rejected:
  one pull request may contribute to several sections, and its
  fragment should stay one reviewable file.
- **An existing tool (towncrier, changesets)** — rejected: each carries
  its own dependency and changelog format; the rotation, the Keep a
  Changelog document, and the release-notes pipeline ([ADR-0618](./0618-release-notes-corpus-and-area-groups.md) §5) are
  ours and stay.
- **A bot appending entries on merge** — rejected: the entry is the
  author's, reviewed in the pull request; a bot moves that review after
  merge and re-introduces a single writer.

## Consequences

Changelog-carrying pull requests stop conflicting; CHANGELOG.md changes
only through the release sweep, the repair label, and the assembly.
Within a released section, entries appear in ascending pull-request
order instead of merge order. The fragment pool is a visible answer to
"what is unreleased". The release-notes generator's input and trust
model are unchanged: fragments are pull-request-reviewed text frozen at
merge, and the released section it reads is assembled before the tag. A
reverted pull request must delete its fragment with the revert — a
stale one rides the next cut, caught by the pool's visibility.
