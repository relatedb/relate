# ADR-0572: Two-commit release — bump push is the cut, stamp commit is the tag target

Status: Proposed
Claim: 2aedc776933c8e837113fa067b0fd77b18e9356f

## Context

The release workflow's one release commit carries both the version bump
and the Swift package stamp (ADR-0433 §6): the job that pushes it first
builds the five-target XCFramework and computes its checksum, so every
other release artifact — CLI binaries, FFI libraries, wheels, the wasm
bundle — waits on that build before it can start. The XCFramework build
is the release critical path's head gate.

Two invariants bound any reshaping. First, the consistency guarantee: a
commit is in the release exactly when it is an ancestor of the tag and
in the tree the artifacts were built from — the successful push of the
release commit is the cut, and nothing may be built before the cut is
known. Second, ADR-0433 §6: the tagged `Package.swift` must pin the
exact zip bytes a SwiftPM consumer downloads at that tag.

## Decision

Split the one release commit into two.

1. **The bump commit** — every version surface and the changelog,
   pushed by a job that builds nothing. Its successful push is the
   release cut, exactly as the single commit's push was before. The job
   keeps the fetch-rebase-retry on a rejected push; a rebase here is
   safe because no artifact has been built yet.
2. **The stamp commit** — a child of the bump commit carrying only the
   `Package.swift` release constants. The XCFramework's per-target
   static libraries are built from the bump commit's tree — each build
   leg checks out that exact commit, so the libraries are correct by
   construction — and the stamp job assembles them into the zip and
   computes the checksum from the assembled bytes. True by
   construction, as before: no artifact is ever reproduced. The stamp
   job pushes under the same rebase-retry, and a rebase that changes
   the tree rebuilds the libraries from the rebased tree before
   re-stamping.
3. **One build source** — every release artifact, the XCFramework zip
   included, is built from the stamp commit; the tag points there. The
   build wave starts after the stamp push, so the tagged tree and the
   built tree cannot diverge, whatever lands on main mid-release.

## Alternatives considered

- **Keep the single commit** (ADR-0433 §6 as written). Rejected: the
  head gate serializes the XCFramework build ahead of every artifact
  build, and the stamp no longer needs to ride the same commit as the
  bump — it only needs to be an ancestor of the tag.
- **Start the build wave from the bump commit and validate later.**
  Rejected: a merge landing between the bump push and the stamp push
  would put commits in the tag's ancestry that are not in the built
  tree — reintroducing exactly the inconsistency the cut exists to
  prevent; the validation would turn it into a failed release and a
  re-dispatch, a worse rare case than a slower common case.
- **Rebuild the XCFramework deterministically after the version
  commit** (the approach ADR-0433 rejected). Still rejected: it needs
  byte-reproducible artifacts across runners. This ADR builds once and
  checksums the assembled bytes; no reproduction is assumed anywhere.
- **Tag a commit that is not on main** (the stamp as a tag-only
  commit). Rejected: main's `Package.swift` would keep stale release
  constants, breaking source checkouts between releases.

## Consequences

- main carries two `chore(release)` commits per release; the tag points
  at the second. Release notes and the registry pipeline resolve the
  tag and are unaffected.
- The bump commit's re-dispatch idempotence is unchanged (empty diff,
  reuse the pushed commit). The stamp commit's re-dispatch re-assembles
  and re-stamps; a byte-different zip stacks a second stamp commit —
  correct, because the checksum always pins the attached zip, but
  occasionally noisy.
- The XCFramework build legs and the artifact build wave no longer
  contend for macOS runners: the legs finish before the wave starts.
- ADR-0433 §6's checksum contract is unchanged; its one-commit shape is
  superseded by this ADR (Later references note added there).
