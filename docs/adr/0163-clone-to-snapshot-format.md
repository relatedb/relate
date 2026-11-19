---
type: adr
area: cli
date: 2026-10-04
deciders: CypherLite maintainers
name: 0163-clone-to-snapshot-format
status: accepted
tags:
  - commands
  - load
title: Clone `--to-snapshot-format` transcodes the checkpoint codec
REFERENCES:
  - "[[adr/0074-single-file-bundle-vfs]]"
  - "[[adr/0159-snapshot-round-trip]]"
  - "[[adr/0161-exclusive-destination-creation]]"
  - "[[adr/0282-clone-covers-dir-bundle-matrix]]"
---

# 0163. Clone `--to-snapshot-format` transcodes the checkpoint codec

## Context

[ADR-0282](./0282-clone-covers-dir-bundle-matrix.md) folded the dir/`.cl` matrix into `cypherlite clone SOURCE DEST`
and one engine API, `Database::clone(source, dest, CloneOptions)`, and its
§4 carried [ADR-0159](./0159-snapshot-round-trip.md)'s codec-preservation rule into the fold: the
destination keeps the source's snapshot codec in every direction. That
rule is the right default, but it makes the codec a one-way door — there
is no way to produce a compact Parquet bundle from a JSON working
directory, or the reverse, short of opening the clone and re-checkpointing
it by hand.

The publish machinery makes an override nearly free.
`publish_into` (`cypherlite/cypherlite/src/publish.rs`) today passes
`Some(source_db.inner.store.snapshot_codec())` as the `requested` codec to
`parse_uri_creating`, and `publish_cut`'s
`checkpoint_at(dest_store, …)` writes through the destination store's
codec. Transcoding is therefore threading an override through
`publish_into`/`publish_cut`, not a new write path. The storage layer
already validates a requested codec before anything is created:
`parse_uri_inner` (`cypherlite/cypherlite-storage/src/lib.rs`) runs
`codec::check_available(codec)?` on the requested codec before
`parse_filesystem_uri` opens or creates the destination, so an uncompiled
codec request fails before the destination exists.

The same review (stellar round 1) that prompted this flag also found two
hand-rolled guards in `clone.rs` that duplicate canonical storage-layer
checks; both moves are recorded here so [ADR-0282](./0282-clone-covers-dir-bundle-matrix.md) stays immutable.

## Decision

1. **`cypherlite clone` gains `--to-snapshot-format json|parquet`.** The
   flag selects the destination store's checkpoint codec. When it is
 absent, the destination keeps the source's codec — the [ADR-0282](./0282-clone-covers-dir-bundle-matrix.md) §4
   default is unchanged.
2. **`CloneOptions` gains `to_snapshot_format: Option<SnapshotCodecKind>`.**
   `None` (the default) preserves the source codec; `Some(kind)`
   transcodes. `publish_into` takes the override and passes it — falling
   back to `Some(source_db.inner.store.snapshot_codec())` when `None` —
   as the `requested` codec to `parse_uri_creating`; `publish_cut`'s
   `checkpoint_at` then writes the destination checkpoint through the
   requested codec. The `debug_assert_eq!` codec equality between source
   and destination in `publish_cut` holds only when no override is set.
3. **An uncompiled codec fails before the destination is created.**
   `parse_uri_inner` runs `codec::check_available` on the requested codec
   before `parse_filesystem_uri` opens or creates anything, so requesting
   a codec the build does not include is an `Error::Storage` with no
 destination left behind — [ADR-0161](./0161-exclusive-destination-creation.md)'s cleanup path is never reached.
4. **The `wal_dir`-with-bundle-source rejection moves into the storage
 layer** (stellar round-1 finding). [ADR-0282](./0282-clone-covers-dir-bundle-matrix.md) §2's contract —
   `CloneOptions.wal_dir` with a `.cl` source is `Error::Storage` — is
   unchanged, but the hand-rolled pre-check in `clone.rs` goes away:
   `parse_filesystem_uri` already rejects
   `is_bundle_path(path) && wal_dir.is_some()` with `Error::Storage`
 ([ADR-0074](./0074-single-file-bundle-vfs.md)), so clone relies on the canonical check instead of
   duplicating it.
5. **The `:memory:`/empty guards move to a canonical helper.** The
   hand-rolled `:memory:`/empty-string checks in `clone.rs` use a
   canonical `is_memory_uri` exported from `cypherlite-storage`, the same
   classification `parse_uri_inner` applies, instead of comparing string
   literals locally.

## Alternatives considered

- **Reusing `--snapshot-format` on clone.** Rejected: the query command's
  `--snapshot-format` means "codec at database creation, empty directory
  only"; the same flag name with a different meaning on clone would
  collide, and PR #165 already dropped it from clone for exactly this
  reason. A distinct `--to-snapshot-format` name keeps the two semantics
  unambiguous.
- **A `--to-format` flag.** Rejected: "format" is ambiguous with the
 dir/`.cl` container choice, which [ADR-0282](./0282-clone-covers-dir-bundle-matrix.md) settled as suffix-inferred
  with no flag. Naming the flag `--to-snapshot-format` says exactly which
  layer it selects — the checkpoint codec, not the container.
- **Always preserving the source codec with no override (status quo).**
  Rejected: moving between codecs is a real need — e.g. producing a
  compact Parquet bundle from a JSON working directory — and the publish
  path makes the override nearly free, since the destination codec is
  already a parameter of `parse_uri_creating`.

## Consequences

- `clone --to-snapshot-format` and `CloneOptions.to_snapshot_format` are
  new public surface: the CLI flag table and `--help` text
  (`docs/api/cli/spec.md`) and the Rust API docs (`docs/api/rust/`) gain
  the option in the same change as the code, per the documentation
  contract.
- `publish_into`/`publish_cut` gain a codec-override parameter; the
  no-override path is byte-identical to today (source codec threaded
  through), so existing clone behavior and tests are unaffected.
- A transcode request for a codec the build does not include fails with
  `Error::Storage` from `codec::check_available` before the destination
 is created — no partial destination, no [ADR-0161](./0161-exclusive-destination-creation.md) cleanup.
- The `wal_dir`-with-`.cl`-source rejection keeps its [ADR-0282](./0282-clone-covers-dir-bundle-matrix.md) contract
  (`Error::Storage`) but is enforced once, canonically, in
 `parse_filesystem_uri` ([ADR-0074](./0074-single-file-bundle-vfs.md)); `clone.rs` loses its hand-rolled
  pre-check and its local `:memory:`/empty literals in favor of
  `cypherlite-storage`'s `is_memory_uri`.
- [ADR-0282](./0282-clone-covers-dir-bundle-matrix.md) stands unchanged at the command/API level; its §4
  codec-preservation rule is now the default rather than the only
  behavior (see its **Later references**).
