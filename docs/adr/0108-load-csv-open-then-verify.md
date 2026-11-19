---
type: adr
area: engine
date: 2026-09-26
deciders: CypherLite maintainers
name: 0108-load-csv-open-then-verify
status: proposed
tags:
  - load
  - security
title: Open-then-verify for local `LOAD CSV` reads
REFERENCES:
  - "[[adr/0081-opt-in-load-csv-file-capability]]"
  - "[[adr/0097-exclusive-storage-ownership]]"
---

# 0108. Open-then-verify for local `LOAD CSV` reads

## Context

[ADR-0081](./0081-opt-in-load-csv-file-capability.md) confines local `LOAD CSV` sources to a configured canonical root,
rejecting lexical traversal, out-of-root symlink targets and non-regular
files. Its `CsvPolicy::target` canonicalizes the candidate target, checks it
lies within the canonical root by path components, and requires a regular
file; `CsvPolicy::read` then opens that path and performs the bounded read.
Inspection and opening are therefore separate operations on a path string: a
concurrent filesystem mutation between the two (for example swapping a
regular in-root file for a symlink to an out-of-root file, or replacing a
symlink with a file) can invalidate the earlier check. [ADR-0081](./0081-opt-in-load-csv-file-capability.md)'s
Consequences records this gap explicitly. Oplane requirement
`OPLANE_REQ-00262972` (primary case #4) rejects the check-then-open sequence
as a non-atomic use of an inspected path; LABS-115 tracks the fix.

[ADR-0097](./0097-exclusive-storage-ownership.md) already needs to compare physical filesystem objects rather than
path spellings, and solves it for storage ownership with a `FileIdentity`
(device and inode on Unix; `GetFileInformationByHandle` volume serial plus
file index on Windows, via a target-specific `windows-sys` dependency) and
`O_NOFOLLOW` opens that never follow the final path component. That precedent
lives in `cypherlite-storage` as a private module, so `LOAD CSV`, which runs
in `cypherlite-executor`, cannot reuse it. The identity approach deliberately
does not distinguish hard links: two names for the same inode compare equal.

## Decision

**`LOAD CSV` records the target's physical identity at validation and
requires the opened handle to match it before reading.**

1. **Record identity when validating.** `CsvPolicy::target` returns the
   canonical path together with the physical `FileIdentity` observed at
   validation time. The existing rejections (out-of-root canonical target,
   non-regular file, malformed URL) are unchanged; recording an identity is
   added to the same inspection, not a second separate check.

2. **Open without following the final component.** `CsvPolicy::read` opens
   the recorded canonical path through `std::fs::OpenOptions` via the shared
   `cypherlite_core::file_identity::nofollow` helper, whose Unix arm adds
   `libc::O_NOFOLLOW` to `custom_flags` and whose Windows arm adds
   `FILE_FLAG_OPEN_REPARSE_POINT`, so a symlink substituted at the final
   component is refused at open on Unix, and opened as the link itself on
   Windows where the identity check in (3) denies it.

3. **Verify the opened handle, then read.** Before the bounded read, require
   `cypherlite_core::file_identity::of_file(&file)` to equal the identity
   recorded in (1), and require the same `fstat` to report a regular file. A
   mismatch, a non-regular opened object, or an identity-query failure denies
   the read. The bounded `limit + 1` read and the per-statement memo are
   unchanged.

4. **Keep denials generic.** Any mismatch denies with the existing generic
   typed `LOAD CSV file unavailable` error; no path, canonical path, errno,
   or OS error text is added.

5. **Share the identity primitive.** Move `file_identity` from
   `cypherlite-storage` to `cypherlite-core` as a `pub` module.
   `cypherlite-core` gains a `cfg(unix)` `libc` target dependency and a
   `cfg(windows)` `windows-sys` target dependency (0.61, `Win32_Foundation`
   and `Win32_Storage_FileSystem`); `cypherlite-storage` drops its
   `windows-sys` target dependency and keeps `libc` for its own no-follow
   opens. The `O_NOFOLLOW` open in `CsvPolicy::read` reuses
   `cypherlite_core::file_identity::nofollow`, so `cypherlite-executor`
   needs no `libc` dependency of its own. The Windows identity remains
   `GetFileInformationByHandle` as chosen
 in [ADR-0097](./0097-exclusive-storage-ownership.md). Declaring these target dependencies in an engine crate is the
   rule R6 record required by AGENTS.md.

This closes the specific inspection/open race for local `LOAD CSV`. It is an
identity-bound open, not a directory-relative capability: the read still
resolves the canonical absolute path.

## Alternatives considered

- **Directory-relative `openat`/`dirfd` component walk with `O_NOFOLLOW` per
  component.** Rejected: it is Unix-only, leaving Windows on a different
  mechanism, and it would reject *every* symlink component, banning the
 in-root symlink aliases [ADR-0081](./0081-opt-in-load-csv-file-capability.md) currently allows. The residual race is a
  final-component replacement, which a single `O_NOFOLLOW` open plus identity
  check already covers without narrowing accepted inputs.
- **Adopt the `cap-std` capability crate.** Rejected: it is a heavy new
  dependency (its own transitive tree and platform layer) for one read site
 in the engine, far larger than the targeted identity primitive [ADR-0097](./0097-exclusive-storage-ownership.md)
  already established.
- **Re-verify through `/proc/self/fd` after opening.** Rejected: `/proc` is
  Linux/macOS-specific and not uniformly available (including containers and
  other Unix hosts), and re-reading the magic link is more fragile than
  comparing the handle's own identity.
- **Record `ACCEPTED_RISK` in Oplane instead of fixing it.** Rejected: the
  fix is small — reuse an existing identity primitive plus one open flag and
 one comparison — and [ADR-0097](./0097-exclusive-storage-ownership.md) already set the precedent and cost for this
  exact class of race.

## Consequences

- The check-then-open race described in [ADR-0081](./0081-opt-in-load-csv-file-capability.md)'s Consequences is closed for
  local `LOAD CSV`: the file that is actually read is the physical object
  inspected, regardless of final-component replacement between validation and
  open. The read-path accept/reject semantics are otherwise unchanged —
  relative paths, rooted `file:///` URLs, the root confinement, the regular
  file requirement, the byte cap and the generic errors all behave as before.
  Only the race is closed.
- A residual pre-existing caveat remains: an in-root hard link to an
  out-of-root inode is still indistinguishable from an in-root file, because
  the recorded identity compares the inode or file index and cannot know how
 many names point at it. This is the same limitation [ADR-0097](./0097-exclusive-storage-ownership.md) accepts for its
  ownership identities, and it is not introduced or worsened here.
- `file_identity` becoming a `pub` module of `cypherlite-core` adds the
  `cfg(unix)` `libc` and `cfg(windows)` `windows-sys` target dependencies to
  core and removes `windows-sys` from storage; `cypherlite-executor` performs
  its `O_NOFOLLOW` open through the shared
  `cypherlite_core::file_identity::nofollow` helper and gains no `libc`
  dependency. Both crates remain within the
  workspace boundary rules; the dependency is a move, not a new external
  tree, and its R6 record is this ADR.
- Tests must cover a final-component symlink swapped between validation and
  open being denied, a matching regular file being accepted, and a
  non-regular opened object being denied; persistence/identity tests
 continue to exercise [ADR-0097](./0097-exclusive-storage-ownership.md) behavior after the module move.
