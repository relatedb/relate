# ADR-0591: The Java client publishes to Maven Central as `ai.relatedb:relate`

Status: Proposed
Claim: 40c7ea664d85aa99bf3d56db5919417b62e5d6f8

## Context

ADR-0490 bundles the native library in the Java client's jars and left
Maven Central publication out of scope. The publish workflow now carries a
dormant Maven job, so the artifact's public coordinates must be fixed
before the first upload: Central treats published coordinates as
permanent, and a namespace has to be verified through ownership of a
domain before anything can be uploaded under it.

The coordinates used so far (`com.relate`) name a domain the project does
not own, so that namespace cannot be verified.

## Decision

- The group is `ai.relatedb` — the reversed project domain, verifiable by a
  DNS record — and the artifact stays `relate`: `ai.relatedb:relate` is
  the fat jar, with one classifier jar per platform as in ADR-0490.
- The Java package moves with the group, from `com.relate` to
  `ai.relatedb`, so the group, the package and the project domain agree
  and the artifact's imports stay predictable. The move is breaking for
  any source importing the old package; nothing was published under the
  old one.
- Publication is a manual dispatch of the registries workflow, signed with
  a project key and uploaded with a Portal token held as environment
  secrets. Central has no trusted-publishing equivalent, so this is the
  one registry that keeps static credentials. Uploads are not auto-released:
  the first versions are validated and released by hand in the Portal.
- The release's FFI archives, verified against the release run's build
  attestation, are the only source of the bundled natives.

## Alternatives considered

- **`com.relate`.** Rejected: the namespace cannot be verified without
  owning the domain.
- **`io.github.relatedb`.** Verifiable without DNS, but it ties a public
  coordinate to the hosting organisation; rejected in favour of the
  project's own domain.
- **JReleaser as the publisher.** Rejected: the pom's own publishing
  plugin already covers signing and upload, and the release process
  already lives in the workflows; a second release tool would duplicate
  it.

## Consequences

- Consumers depend on `ai.relatedb:relate` and import `ai.relatedb`;
  every document naming the old coordinates or package moves with this
  change.
- The namespace verification, the Portal account and the signing key are
  owner actions with a runbook in the publishing guide; the job stays
  dormant until they exist.
- A published version cannot be changed or removed, so a bad upload is
  fixed by the next version.
