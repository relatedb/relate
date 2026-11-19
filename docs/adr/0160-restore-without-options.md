---
type: adr
area: engine
date: 2026-10-03
deciders: CypherLite maintainers
name: 0160-restore-without-options
status: accepted
tags:
  - storage
title: Restore from a `.cl` bundle takes no options
REFERENCES:
  - "[[adr/0159-snapshot-round-trip]]"
  - "[[adr/0282-clone-covers-dir-bundle-matrix]]"
SUPERSEDES:
  - "[[adr/0159-snapshot-round-trip]]"
---

# 0160. Restore from a `.cl` bundle takes no options

## Context

[ADR-0159](./0159-snapshot-round-trip.md) gave both bundle-crossing operations the same options type:
`snapshot_to_bundle(source, dest, SnapshotOptions)` and
`restore_from_bundle(source, dest, SnapshotOptions)`. `SnapshotOptions` has
one field, `wal_dir: Option<PathBuf>`, naming the source's external WAL. A
`.cl` bundle embeds its WAL, so `restore_from_bundle` rejected every `Some`
value at runtime (`wal_dir is not supported when restoring from a .cl
bundle`) and every caller passed `SnapshotOptions::default()` or
`SnapshotOptions { wal_dir: None }`. The signature made an invalid state
representable: the only expressible choice beyond the default was a
guaranteed error.

## Decision

`Database::restore_from_bundle(source, dest)` takes no options.
`SnapshotOptions` stays on `snapshot_to_bundle` only, where `wal_dir` names
the directory source's external WAL (mirroring `CloneOptions`). The runtime
rejection goes away with the parameter: there is no `wal_dir` left to
reject.

## Alternatives considered

- **Keep the parameter and reject `Some` at runtime** (the [ADR-0159](./0159-snapshot-round-trip.md) status
  quo). Rejected: the type admits a value that can never succeed, so every
  caller must know a rule the signature could have stated. Making the
  invalid state unrepresentable removes both the rule and the error path.
- **Split per-direction options types** (`SnapshotOptions` and an empty
  `RestoreOptions`). Rejected: an empty options struct is speculation, not
  design — YAGNI until restore actually needs an option. A future need can
  add `RestoreOptions` in its own ADR.

## Consequences

- `restore_from_bundle(source, dest)` call sites (the CLI's
  `snapshot --from`, the Rust example, the tests) drop the
  `SnapshotOptions` argument.
- `SnapshotOptions.wal_dir` is documented as used only by
 `snapshot_to_bundle`; [ADR-0159](./0159-snapshot-round-trip.md) §5 is superseded on this point (see its
  **Later references**).
- The `wal_dir is not supported when restoring from a .cl bundle` error and
  its test coverage are removed. The CLI already refuses `--wal-dir` with
  `--from` at parse time, so the user-facing contract is unchanged.
- If restore grows a real option later, a new ADR adds a `RestoreOptions`
  parameter.

## Later references

- [ADR-0282](./0282-clone-covers-dir-bundle-matrix.md) removes
  `restore_from_bundle` along with the whole snapshot surface, folding the
  dir/`.cl` matrix into `Database::clone(source, dest, CloneOptions)`. The
  rationale here — a `.cl` bundle embeds its WAL — carries into the fold:
  `CloneOptions.wal_dir` is rejected with `Error::Storage` when the source
  is a `.cl` bundle. This ADR is superseded at the API level.
