---
type: adr
area: cli
date: 2026-10-04
deciders: CypherLite maintainers
name: 0282-clone-covers-dir-bundle-matrix
status: accepted
tags:
  - commands
  - load
title: Clone covers the dir/`.cl` matrix; the `snapshot` command is removed
REFERENCES:
  - "[[adr/0074-single-file-bundle-vfs]]"
  - "[[adr/0136-read-only-open]]"
  - "[[adr/0156-directory-clone]]"
  - "[[adr/0159-snapshot-round-trip]]"
  - "[[adr/0160-restore-without-options]]"
  - "[[adr/0161-exclusive-destination-creation]]"
  - "[[adr/0163-clone-to-snapshot-format]]"
SUPERSEDES:
  - "[[adr/0159-snapshot-round-trip]]"
  - "[[adr/0160-restore-without-options]]"
---

# 0282. Clone covers the dir/`.cl` matrix; the `snapshot` command is removed

- **Renumbered**: from 0162 (shared-number cleanup)

## Context

[ADR-0156](./0156-directory-clone.md) gave `cypherlite clone SOURCE DEST` directory → directory copies.
[ADR-0159](./0159-snapshot-round-trip.md) then added a separate lifecycle verb `cypherlite snapshot DATABASE
(--to FILE.cl | --from FILE.cl)` plus engine APIs
`Database::snapshot_to_bundle` / `Database::restore_from_bundle` /
`SnapshotOptions` to cross the dir/`.cl` boundary (shipped in v0.11.2, PR
#173, commit 0a1271c1). [ADR-0160](./0160-restore-without-options.md) narrowed `restore_from_bundle` to take no
options because a `.cl` bundle embeds its WAL, and [ADR-0161](./0161-exclusive-destination-creation.md) made publish
destination creation exclusive and crash-safe for clone, snapshot, and
restore alike.

Linear LABS-168: two verbs doing one job — moving a database from one store
to another — is the wrong surface. `clone` and `snapshot` differ only in
which formats the endpoints may have; the publish machinery underneath is
already shared. The snapshot work has shipped in internal releases only, so
there is no backwards-compatibility or deprecation burden: the surface can
simply break. The fold is surgical, not a revert: the shared publish path
(`cypherlite/cypherlite/src/publish.rs`), the storage `OpenMode`/`OpenAdmit`
layer, and [ADR-0161](./0161-exclusive-destination-creation.md)'s exclusive destination creation all stay, because clone
already depends on them.

## Decision

1. **`cypherlite clone SOURCE DEST` covers the full dir/`.cl` matrix**:
 dir → dir (unchanged, [ADR-0156](./0156-directory-clone.md)), dir → `.cl`, `.cl` → dir, and
   `.cl` → `.cl`. The format of each side is inferred from the `.cl`
   suffix on its path — the same classification `Database::open` applies
   everywhere — so there is no `--format` flag.
2. **One engine API: `Database::clone(source, dest, CloneOptions)`.** It
   replaces `clone_directory`, `snapshot_to_bundle`, and
   `restore_from_bundle`; `SnapshotOptions` goes away with them.
   `CloneOptions.wal_dir` names the source's external WAL and is rejected
   with `Error::Storage` when the source is a `.cl` bundle — a bundle
 embeds its WAL, so [ADR-0160](./0160-restore-without-options.md)'s rationale carries into the fold.
3. **The `snapshot` command and its engine API are removed outright.** The
   command leaves `COMMAND_WORDS` in
   `surfaces/cypherlite-cli/src/flags.rs`, the `--to`/`--from` snapshot
   parsing and docs rows go with it, and there are no deprecation shims or
   migration hints: the verb shipped in internal releases only, so it
 breaks cleanly. [ADR-0159](./0159-snapshot-round-trip.md) and [ADR-0160](./0160-restore-without-options.md) are superseded at the command and
   API level (see their **Later references**).
4. **The shared machinery stays.** `publish.rs`, the storage
 `OpenMode`/`OpenAdmit` layer, the [ADR-0136](./0136-read-only-open.md) frozen read-only source
 open, and [ADR-0161](./0161-exclusive-destination-creation.md)'s exclusive, crash-safe destination creation are
   unchanged; clone keeps depending on them in all four directions. The
   destination keeps the source's snapshot codec in every direction
 ([ADR-0159](./0159-snapshot-round-trip.md) §4 carries over).
5. **The message verb is `clone` everywhere** — errors, usage text, and
   the `DESTINATION_EXISTS`-prefixed publish messages name `clone`, never
   `snapshot`.

## Alternatives considered

- **Keeping `snapshot` as a separate verb beside `clone`.** Rejected: the
  two verbs share one publish path and differ only in permitted endpoint
  formats, so users would have to learn a distinction the implementation
  does not make. One verb with suffix-inferred formats is strictly simpler
  and removes a whole command's worth of flags, docs, and tests.
- **An explicit `--format dir|bundle` flag on either side.** Rejected: the
  `.cl` suffix already classifies every store path in the product
  (`Database::open` infers format the same way), so a flag would duplicate
  information the user already gave — and could contradict it. Inference
  keeps `clone SOURCE DEST` to the clig.dev single-positional shape with
  no new syntax.
- **A literal `git revert` of 0a1271c1.** Rejected: the revert would also
  remove the shared publish path, the storage `OpenMode`/`OpenAdmit`
 layer, and [ADR-0161](./0161-exclusive-destination-creation.md)'s exclusive destination creation, all of which clone
  now depends on. The fold removes only the snapshot surface — command,
  engine API, docs rows — and keeps the machinery.

## Consequences

- `clone` is the single verb for moving a database between any two stores;
  `cypherlite snapshot …` fails with the generic unknown-command error.
- A database path literally named `snapshot` becomes openable again: once
  the word leaves `COMMAND_WORDS` it no longer shadows such a path as the
  first argument (the `./snapshot` / `-- snapshot` escapes are no longer
  needed).
- The unrelated query flag `--snapshot-format` (snapshot codec selection
  at database creation) is unaffected and stays.
- `Database::clone_directory`, `snapshot_to_bundle`,
  `restore_from_bundle`, and `SnapshotOptions` are removed from the public
  Rust API in one breaking change; internal-release consumers move to
  `Database::clone`.
- `.cl` → `.cl` copy is now spelled `clone a.cl b.cl`; the [ADR-0159](./0159-snapshot-round-trip.md) note
  that it stays "checkpoint, then copy the file" is superseded by the
  unified publish path.
- [ADR-0159](./0159-snapshot-round-trip.md) and [ADR-0160](./0160-restore-without-options.md) remain as history, superseded at the command/API
 level; [ADR-0161](./0161-exclusive-destination-creation.md) stands unchanged.

## Later references

- [ADR-0163](./0163-clone-to-snapshot-format.md) adds
  `clone --to-snapshot-format json|parquet` and
  `CloneOptions.to_snapshot_format`, making §4's codec preservation the
  default rather than the only behavior, and moves §2's
  `wal_dir`-with-bundle-source rejection from a hand-rolled `clone.rs`
 pre-check into the canonical storage-layer check ([ADR-0074](./0074-single-file-bundle-vfs.md)), keeping
  the `Error::Storage` contract.
