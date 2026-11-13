# ADR-0592: The Go client is a nested module that carries its header and links a consumer-supplied native library

Status: Proposed
Claim: 1419e60e0f50ea9b89a870905ad0eba20e3e7132

## Context

The Go client wraps the C ABI through cgo (ADR-0502). Go has no registry to
publish to: the module proxy serves a module zip built from a repository
tag. The client lives in a subdirectory of the repository, so two things
broke consumption from outside the repository:

- the module is nested, and the Go toolchain resolves a nested module only
  from a tag that carries the module's directory as a prefix;
- the cgo directives included the header from, and linked the library
  out of, paths above the module directory. A module zip contains only the
  module directory, so neither existed for a consumer.

## Decision

- The module ships its own copy of the C header, included relative to the
  module. A test fails whenever the copy differs from the canonical header,
  so the two cannot drift.
- The module does not bundle the native library. Consumers fetch the
  release's per-platform FFI archive (library plus header, the same
  attested archives the other clients consume), and point the linker at it
  through the standard cgo linker environment. In-repository builds keep
  resolving the library from the build tree.
- Each release also pushes a second tag with the module's directory
  prefix, on the same commit as the release tag, so the module version
  equals the release version.

## Alternatives considered

- **Commit prebuilt static libraries per platform into the module or
  into per-platform modules.** Gives a zero-configuration `go get`, but
  adds tens of megabytes per platform to the repository history on every
  release and a second publication path. Rejected for now; it can be added
  later without changing this decision's tag or header rules.
- **GoReleaser.** Rejected: it builds and publishes binaries, and the Go
  client is a library with no binary to release.
- **A separate repository holding the Go client.** Rejected: it forks the
  client from the ABI it must track and needs a sync step on every change.

## Consequences

- `go get` fetches and compiles the module; linking needs the release
  archive's library on the linker path, documented in the Go spec and the
  client reference.
- The header copy is a second file to keep in step; the drift test is the
  guard.
- The tag step in the release workflow is outward-facing: it pushes a
  second tag for every release.
