---
type: adr
area: engine
date: 2026-10-03
deciders: CypherLite maintainers
name: 0161-exclusive-destination-creation
status: accepted
tags:
  - storage
title: Exclusive, crash-safe publish destination creation
REFERENCES:
  - "[[adr/0136-read-only-open]]"
  - "[[adr/0156-directory-clone]]"
  - "[[adr/0159-snapshot-round-trip]]"
---

# 0161. Exclusive, crash-safe publish destination creation

## Context

Clone ([ADR-0156](./0156-directory-clone.md)) and snapshot/restore ([ADR-0159](./0159-snapshot-round-trip.md)) publish into a destination
that "must not exist". The enforcement was check-then-create:
`publish::reject_destination` stats the destination, and the create side
afterwards *adopts* whatever is there:

- `NativeVfs::open_regular_file` opens with `create_new(true)` but falls
  back to `open_existing_regular` on `AlreadyExists`, so a `.cl` host file
  that appears in the window is opened `O_RDWR` — or fails its
  `try_lock_exclusive` with `Error::Locked` while the winner still writes.
- `resolve_bundle_path` canonicalizes a symlinked final component,
  following a link raced into the window.
- Native directory creation adopts a pre-existing directory
  (`create_private_dir` reports `created = false` and the lease proceeds).

The window is exploitable. Two concurrent `cypherlite snapshot db --to
x.cl` runs both pass the pre-check; run 1 creates `x.cl` and holds
`LOCK_EX`; run 2's open falls back to run 1's file, fails the lock, and
then `publish::remove_created` **deletes run 1's valid in-progress bundle**
— flock is advisory and unlink ignores it. A local attacker racing a
symlink into the window gets the victim opened `O_RDWR` or deleted. The
trust boundary is `remove_created`'s documented invariant "the destination
did not exist before this call", which the check-then-create flow never
enforced.

A second, smaller defect: `remove_created` runs while the destination
store handle is still alive, holding the flock and open file handles. On
Windows an open handle blocks the unlink, so partial-destination cleanup
fails there.

## Decision

1. **Publish destination creation is create-exclusive end to end.** A new
   `cypherlite_storage::parse_uri_creating` (beside `parse_uri_opening`)
   drives it; `publish_into` uses it for clone, snapshot, and restore.
   - `.cl` bundles: `BundleVfs::open_create_exclusive` creates the host
     file with `create_new(true)` and **no `AlreadyExists` fallback**;
     `resolve_bundle_path` never canonicalizes a symlinked final
     component in this mode. An existing entry — file, directory, or a
     symlink raced into the window — fails with an `Error::Storage`
     whose message starts with the exported
     `cypherlite_storage::DESTINATION_EXISTS` prefix
     ("destination already exists").
   - Native directories: `native_ownership::lease_creating` requires the
     final component to be created by this call
     (`create_private_dir_exclusive`); a pre-existing (or raced-in, or
     symlinked) final component fails with the same
     destination-already-exists storage error instead of being adopted.
2. **Ordinary opens are unchanged.** `parse_uri`, `parse_uri_opening`,
   `BundleVfs::open_or_create`, and `Database::open` keep adopting
   existing stores; only the publish path opts into exclusivity.
3. **`remove_created`'s invariant is enforced by construction.** When the
   exclusive create reports destination-already-exists, publish created
   nothing, so `publish_into` returns the error (re-wrapped with the
   verb, e.g. "snapshot destination already exists: PATH") **without**
   calling `remove_created`. The loser of a create race deletes nothing;
   the winner's in-progress destination survives byte-identical. The
   `reject_destination` pre-check stays as a fast path with the
   verb-naming message.
4. **The destination store handle is dropped before `remove_created`.**
   In the publish error paths the store (flock + open handles) is dropped
   first, so partial-destination cleanup also succeeds on Windows.

## Alternatives considered

- **Stage to a temp path, then rename over the final name.** Rejected:
  it would make the `.cl` case fully atomic (no partial bundle ever
  visible at the final name), but exclusive create already restores
  `remove_created`'s invariant — the security finding is adoption and
  deletion of *someone else's* destination, which `O_EXCL` semantics
  close completely. Crash residue is a smaller, documented contract: a
  crashed publish leaves a partial destination that the next run refuses
  as "already exists" and the user deletes explicitly — unchanged from
 [ADR-0156](./0156-directory-clone.md)/[ADR-0159](./0159-snapshot-round-trip.md). Staging would add temp-name allocation,
  same-filesystem constraints, and temp cleanup on crash for both
  formats without changing that contract.
- **Keep check-then-create and re-verify file identity after the open.**
  Rejected: flock is advisory and unlink ignores it, so no post-open
  identity check can make the loser's `remove_created` safe. The loser
  must never reach removal, which only exclusive create guarantees.
- **A dedicated `Error::AlreadyExists` variant.** Rejected: a new public
  error variant ripples through the engine, FFI, and CLI error mapping
  for no behavioral gain; the stable `DESTINATION_EXISTS` message prefix
  on `Error::Storage` is sufficient inside the workspace and keeps the
  CLI's "already exists" output stable.
- **Delete on `Error::Locked` too.** Rejected: a lock failure after a
  successful exclusive create means another opener touched the file in
  the microsecond window; unlinking a lock inode another owner holds can
 admit two concurrent owners (the [ADR-0136](./0136-read-only-open.md) warning). Today's
  `remove_created` behavior on that pathological path is retained, but
  the already-exists loser — the realistic race — never reaches it.

## Consequences

- Two concurrent publishes to one destination: the loser is refused with
  destination-already-exists (or `Error::Locked`) and removes nothing;
  the winner's destination is byte-identical afterwards. Covered by tests
  for both `.cl` and native-directory destinations.
- A symlink raced into the create window is never followed and never
  deleted; the victim behind it is untouched.
- Partial-destination cleanup after a failed publish now works on
  Windows (handle dropped before removal).
- The public surface of `cypherlite-storage` grows
  `parse_uri_creating`, `BundleVfs::open_create_exclusive`, and the
  `DESTINATION_EXISTS` prefix constant; all are workspace-internal APIs
  documented in rustdoc.
- `Database::open` of an existing `.cl` bundle or native directory is
  unaffected.
