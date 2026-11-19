---
type: adr
area: engine
date: 2026-10-03
deciders: CypherLite maintainers
name: 0159-snapshot-round-trip
status: accepted
tags:
  - load
  - storage
title: Snapshot round trip between a native directory and a `.cl` bundle
REFERENCES:
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0074-single-file-bundle-vfs]]"
  - "[[adr/0104-cargo-feature-bundles]]"
  - "[[adr/0136-read-only-open]]"
  - "[[adr/0156-directory-clone]]"
  - "[[adr/0158-streaming-single-process-pull]]"
  - "[[adr/0160-restore-without-options]]"
  - "[[adr/0282-clone-covers-dir-bundle-matrix]]"
---

# 0159. Snapshot round trip between a native directory and a `.cl` bundle

## Context

Linear LABS-167: a database must be able to travel from a native directory
to a `.cl` bundle and back — dir → `.cl` → dir — so users do not get stuck
in a format. The `.cl` single-file bundle exists ([ADR-0074](./0074-single-file-bundle-vfs.md)), but today
there is no way to move a database between a native directory and a
bundle: `clone` ([ADR-0156](./0156-directory-clone.md)) is directory → directory only, and pull
([ADR-0158](./0158-streaming-single-process-pull.md)) imports a remote Neo4j/Aura query result, not a local store.

The safe-read pattern is already proven. [ADR-0136](./0136-read-only-open.md) gives a frozen read-only
open beside a live writer: stable-cut manifest retry, bounded reads,
fail-closed on corruption, and no writes to or exclusive lock on the
source. [ADR-0156](./0156-directory-clone.md) publishes that loaded cut into a new store via
`checkpoint_at` plus `init_empty_wal`, refusing existing destinations and
removing a partially created one on failure. Snapshot/restore reuses both
halves across the dir/`.cl` boundary.

## Decision

1. **One new CLI lifecycle verb `snapshot`.** Following [ADR-0069](./0069-cli-lifecycle-commands.md), it is an
   exit-after positional command recognized only as the first argument.
   Its single positional argument is the native directory `DATABASE`, and
   exactly one of two direction flags is required:
   - `cypherlite snapshot DATABASE --to FILE.cl` exports a native
     directory database to a single-file `.cl` bundle.
   - `cypherlite snapshot DATABASE --from FILE.cl` imports a `.cl` bundle
     into a fresh native directory `DATABASE`.
   - `--wal-dir DIR` is allowed only with `--to` (the external WAL of the
     directory source, mirroring `clone`); combining it with `--from` is a
     usage error. Usage errors exit 2; runtime errors exit 1, mirroring
     `clone` and `pull`.
2. **The source is always opened read-only via the [ADR-0136](./0136-read-only-open.md) path**
   (`OpenMode::ReadOnly` / `open_store_readonly`): stable-cut manifest
   retry, bounded reads, fail-closed on corruption. The command never
   writes to or exclusively locks the source. A writer-held `.cl` source
   fails with a lock error, since a bundle writer holds `LOCK_EX` on the
 host file ([ADR-0136](./0136-read-only-open.md)).
3. **The destination must not exist** — the same contract as `clone`
 ([ADR-0156](./0156-directory-clone.md)). A failed run removes any partially created destination,
   file or directory.
4. **The destination keeps the source's snapshot codec in both
   directions** — json stays json, parquet stays parquet. There is no
   codec conversion.
5. **Engine: two new public `Database` methods.**
   `snapshot_to_bundle(source, dest, SnapshotOptions)` and
   `restore_from_bundle(source, dest, SnapshotOptions)` generalize the
   `clone.rs` publish path across the dir/`.cl` boundary. `SnapshotOptions`
   carries `wal_dir: Option<PathBuf>`, used only by `snapshot_to_bundle`.
6. **The command is compiled into every CLI bundle like `clone` — no new
 Cargo feature.** This deviates from the [ADR-0104](./0104-cargo-feature-bundles.md) rule that a new
   capability gets its own feature: snapshot/restore is as fundamental to
   moving data as clone, adds no dependencies, and keeping it
   always-available avoids a bundle in which a user's backup tool
   vanishes.

## Alternatives considered

- **Two verbs `snapshot`/`restore` with SOURCE DEST positionals
  (clone-style).** Rejected: the one-verb form keeps a single positional
  (the clig.dev convention in AGENTS.md §3), and the direction flags make
  the data-flow direction explicit at the call site.
- **Dedicated Cargo feature in `full` only** (the [ADR-0104](./0104-cargo-feature-bundles.md) default for new
  capabilities). Rejected: the command adds no dependencies and is as core
  as `clone`, which is ungated; an `agent`/`minimal` bundle that cannot
  restore a backup would be a trap.
- **`--force` overwrite of an existing destination.** Rejected: a
  backup/restore tool must never destroy data by default; refuse like
 `clone` ([ADR-0156](./0156-directory-clone.md)).
- **`--snapshot-format` codec conversion flag on snapshot.** Rejected:
  round-trip fidelity is the goal; codec conversion is a separate concern
  that can land later without changing this command's shape.
- **Copying files instead of re-serializing** (checkpoint-then-copy for
  dir → `.cl`). Rejected: re-serializing through `checkpoint_at` into a
  fresh store target reuses the proven clone publish path and never copies
  a possibly-dirty WAL.

## Consequences

- Users can move a database dir → `.cl` → dir and are no longer stuck in
  either format.
- `.cl` → `.cl` stays "checkpoint, then copy the file" ([ADR-0156](./0156-directory-clone.md));
  dir → dir stays `clone`.
- The `minimal` and `agent` CLI bundles grow one dependency-free command
 ([ADR-0104](./0104-cargo-feature-bundles.md)).
- Pull ([ADR-0158](./0158-streaming-single-process-pull.md)) is unaffected: it imports remote query results, not a
  local store.
- A future codec-conversion flag or `--force` can be added without
  breaking the command shape.

## Later references

- [ADR-0160](./0160-restore-without-options.md) drops the `SnapshotOptions`
  parameter from `restore_from_bundle`: its only field, `wal_dir`, was
  always rejected (a bundle embeds its WAL), so the signature made an
  invalid state representable. §5 is superseded on this point.
- [ADR-0282](./0282-clone-covers-dir-bundle-matrix.md) folds the
  dir/`.cl` matrix into `cypherlite clone SOURCE DEST` and
  `Database::clone(source, dest, CloneOptions)`, removing the `snapshot`
  command and the `snapshot_to_bundle` / `restore_from_bundle` /
  `SnapshotOptions` engine API outright. This ADR is superseded at the
  command and API level; its §4 codec-preservation rule carries over to
  clone.
