# ADR-0580: Post-release workspace versioning

Status: Proposed
Claim: c6cf979a3cfc899cbb774b8ce0059a62cf936eb7

## Context

Releases are cut by the release pipeline's manual dispatch
(ADR-0067): its version job sweeps every version surface to the next
tag (`scripts/release-bump.sh`), pushes the bump commit to main, and
the tag job tags that commit. ADR-0560's registry distribution then
publishes the 17-crate closure to crates.io in topological dependency
order, lockstep with the tag — a separate dispatch, so the registries
can lag the release. The next tag is computed from git tags
(`scripts/release-next-version.sh`), never from the workspace
manifest: the manifest version is a packaging surface, not the release
source of truth.

After the bump commit lands, main carries the released version until
the next release is cut — the package-drift window. In it the manifest
and the registry sit at the same version, or one pending dispatch
apart, whether the release ran clean or a step was missed: the
comparison carries no signal, and a publish from a main checkout
targets a version the registry already owns or is about to.

## Decision

Main never carries a released version. The tagged release commit is
the one transient carrier — it must hold the version it releases — and
the same release run advances main past it: after each release the
workspace advances to the next unreleased plain-semver version, no
pre-release marker. The advance is a release-bot step in release.yml,
performing the version-surface sweep the bump performs without the
changelog rotation — an advance is not a release cut, and rotating
then would mint a release section for a version that never shipped —
so the released version exists only on the tagged commit and on the
registries, and main's manifest stays disjoint from the published set.

Registry-consistent verification — checking that what the registries
hold matches what the workspace declares — runs only in the release
pipeline's ordered publish, the one place the two are meant to agree.
It is never a standing lane on main: between releases they are
deliberately apart, so a main-based check would report the invariant
itself as drift.

## Alternatives considered

- **Keep main at the released version; add a reporting-only drift
  lane.** Rejected: the window is structural — every inter-release
  commit sits at the released version — so the lane reports on every
  run, a real miss is indistinguishable from the noise, and reporting
  cannot close the ambiguity it observes.
- **Advance via a runbook checklist item.** Rejected: the advance
  rides every release and is mechanical (the bump's own sweep); a
  manual step is skipped under pressure, nothing detects the omission
  until the next release collides with the registry, and the pipeline
  already automates this class of step as the release-bot bump commit.
- **Carry a pre-release marker on main (X.Y.Z-alpha.N).** Rejected:
  one version string spans every surface and registry — the sweep's
  own invariant that stable versions need no PEP 440 translation —
  and pre-release spellings diverge per ecosystem (PEP 440 vs
  semver), splitting the sweep into per-registry translations; the
  next tag comes from git tags, so the marker would buy nothing the
  plain advance lacks.

## Consequences

- The advance step lands in release.yml after the tag, so the release
  commit stays the tagged tip and main's next commit carries the
  unreleased version.
- The manifest version on main is informational between releases: the
  next release still computes its tag from git tags and sweeps the
  surfaces at cut time, so the advanced version is a floor, never a
  prediction of the next tag.
- Publishes from a main checkout can no longer collide with the
  registry: every workspace version is unpublished by construction.
- ADR-0067's release policy is extended, not changed — trigger shape,
  tag-after-build ordering, and lockstep versioning are untouched.
