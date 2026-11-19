---
type: adr
area: engine
date: 2026-09-28
deciders: CypherLite maintainers
name: 0127-store-version-compatibility
status: proposed
tags:
  - storage
title: "Store-version compatibility contract: migrate-on-open, hands-off, forward-only"
REFERENCES:
  - "[[adr/0005-lpg-parquet-layout]]"
  - "[[adr/0022-id-window-checkpoint-shards]]"
  - "[[adr/0059-wal-redo-typed-payloads]]"
  - "[[adr/0074-single-file-bundle-vfs]]"
  - "[[adr/0089-immutable-checkpoint-shard-publication]]"
  - "[[adr/0090-native-reset-role-and-durability]]"
  - "[[adr/0092-manifest-bound-pull-identity]]"
  - "[[adr/0114-wal-recovery-rejects-undecodable-records]]"
  - "[[adr/0120-ontology-document-persistence]]"
  - "[[adr/0136-read-only-open]]"
SUPERSEDES:
  - "[[adr/0059-wal-redo-typed-payloads]]"
---

# 0127. Store-version compatibility contract: migrate-on-open, hands-off, forward-only

## Context

A CypherLite store carries a version marker in every durable subsystem, but
the knowledge of which versions are readable is smeared across the code: the
Parquet manifest's `format_version` is checked in
`cypherlite/cypherlite-storage/src/snapshot/native/mod.rs` (`FORMAT_VERSION =
3` at line 28; `read_manifest` accepts exactly 1 to 3, with v1/v2 falling back
to the monolithic `nodes.parquet`/`relationships.parquet` pair of [ADR-0005](./0005-lpg-parquet-layout.md),
pre-[ADR-0022](./0022-id-window-checkpoint-shards.md) shards); the JSON codec's `graph.json` `value_encoding` is
checked in `cypherlite/cypherlite-storage/src/codec/json.rs` (absent means 1,
2 is accepted, anything else fails); the WAL segment magic `MDBWAL01` is
checked in `cypherlite/cypherlite-storage/src/wal.rs:17`; and the `.cl`
bundle's superblock magic `MDBCLB01` with `VERSION = 1` is checked in
`cypherlite/cypherlite-storage/src/vfs/bundle.rs:31`. There is no registry
that names the current and minimum readable version per subsystem, no
migration table, and no migrate step in `open_store`
(`cypherlite/cypherlite-storage/src/lib.rs:502`).

[ADR-0059](./0059-wal-redo-typed-payloads.md) Decision 4 declined WAL payload compatibility as a pre-1.0
exception, and its own Consequences say a post-1.0 WAL shape change "needs
its own ADR and an explicit compatibility story; this ADR's loud-failure
stance is the pre-1.0 exception, not the permanent rule." [ADR-0114](./0114-wal-recovery-rejects-undecodable-records.md) then made
a checksum-valid but undecodable WAL record a hard error in every position
and ordered `open_store` to validate the snapshot's version markers before
touching any WAL byte. [ADR-0120](./0120-ontology-document-persistence.md) (Accepted, not yet implemented) is the first
consumer of a real compatibility story: it reserves WAL magic `MDBWAL02`,
manifest `format_version: 4` and JSON `value_encoding: 3` for ontology
stores, and defines that older binaries fail at the unknown marker before
modifying data.

Every tagged release v0.4.0 through v0.8.1 writes `FORMAT_VERSION = 3`, so
the v1/v2 manifest readers cover pre-release stores only. The [ADR-0114](./0114-wal-recovery-rejects-undecodable-records.md)
golden stores under `testdata/stores/v0.8.0/` (Parquet directory, JSON
directory, `.cl` bundle) are the compatibility fixtures, exercised by
`cypherlite/cypherlite/tests/golden_stores.rs`.

Because snapshots are rewritten wholesale at every checkpoint, migrating an
old store is almost never a format transform: it is *load with the legacy
reader, replay the WAL, then checkpoint with the current writer*. This ADR
defines the post-1.0 compatibility contract around that observation. It
supersedes [ADR-0059](./0059-wal-redo-typed-payloads.md) Decision 4's pre-1.0 rationale.

Note on this ADR's own history: the initial draft of this Proposed ADR
specified a hop table — a chain of single-hop pure functions over decoded
state, one per version step per subsystem. The end-of-feature audit of the
implementation found that model to be ceremony (no hop ever touched data;
the real legacy dispatch already lived in the codec loaders) and this ADR
was amended in place, while still Proposed, to record the
reader-per-version model the audit prescribed. The rejected hop-table
design is kept under Alternatives considered.

## Decision

The policy is **migrate-on-open: hands-off, forward-only**. Opening a store
whose versions are readable but not current upgrades it in place, without
any user action, before a single query is served. There is no downgrade
path.

1. **One version registry in `cypherlite-storage`.** A single module names,
   per subsystem, `CURRENT_VERSION` (what this binary writes) and
   `MIN_READABLE_VERSION` (the oldest version this binary can load), plus
   the full readable set between them. The initial registry:

   | Subsystem | Version marker | `CURRENT_VERSION` | `MIN_READABLE_VERSION` | Readable set |
   | --- | --- | --- | --- | --- |
   | Parquet snapshot | manifest `format_version` | 3 | 1 | {1, 2, 3} |
   | JSON snapshot | `graph.json` `value_encoding` | 2 | 1 (absent) | {absent, 2} |
   | WAL | segment magic | `MDBWAL01` | `MDBWAL01` | {`MDBWAL01`} |
   | Bundle container | superblock magic + `VERSION` | `MDBCLB01` / 1 | `MDBCLB01` / 1 | {`MDBCLB01`, `VERSION` 1} |

   The JSON codec's version dimension **is** `value_encoding`; the JSON
   envelope gets no separate `format_version` field. Future format ADRs
   change this registry and nothing else to declare a new version; when
 [ADR-0120](./0120-ontology-document-persistence.md) is implemented it adds `format_version` 4, `value_encoding` 3
   and `MDBWAL02` as readable (and, per its content-follows-format rule,
   `MDBWAL02` as the writable magic for segments that hold ontology ops).

2. **Detection is the marker the loader already decoded.** The codec
   loaders decode the version marker as part of their ordinary work, so
   detection costs no extra I/O: there is no separate pre-read of
   `manifest.json`, no re-read/re-parse of `graph.json`, and no standalone
   WAL header scan. Each loader reports the marker it decoded, and one
   per-marker classification in the registry —
   `Current | Legacy(version) | Unreadable` — backs every gate: the
 [ADR-0114](./0114-wal-recovery-rejects-undecodable-records.md) unknown-version rejection, the ownership admission checks, and
   migrate-on-open all consult the same classification. There is no opt-in
   flag: on a current store the classification is `Current` everywhere and
 migration is a no-op. The [ADR-0114](./0114-wal-recovery-rejects-undecodable-records.md) ordering is preserved — every
   subsystem's version markers are read and validated **before any byte is
   mutated**. Only after all markers are known-readable does migration
   begin.

3. **One legacy reader per readable version, plus the current writer.**
   There is no hop table and no chain of single-hop pure functions. For
   every version in the readable set the registry names the legacy reader
   that decodes it (the existing v1/v2 manifest fallback is the first
   instance); the only writer is the current writer. Migrating a store at
   `MIN_READABLE_VERSION` is not a composition of transforms: it is load
   with the named legacy reader, then write with the current writer. The
   codec loaders return the decoded marker alongside the decoded state, so
   the version is named where the loader knows it. Each legacy reader is
   unit testable on its own and removable when `MIN_READABLE_VERSION`
   advances past it; per-version reader tests are the compatibility proof.

4. **Execution: one open flow, then checkpoint + rotate + recycle on the
   same handles.** Migration is the ordinary open sequence — load with the
   reader for the reported marker, `Wal::open`, replay — followed, only
   when a loader reported a legacy marker and the VFS is durable, by a
   checkpoint at `CURRENT_VERSION`, a WAL rotation to a fresh segment with
   the current magic, and recycling of the legacy segments, all on the
   handles already open. There is no second load, no second `Wal::open`,
   no second replay. Before any query is served the store is at current
   format. Publication follows the manifest-last tmp/rename rules of
 [ADR-0089](./0089-immutable-checkpoint-shard-publication.md) and [ADR-0092](./0092-manifest-bound-pull-identity.md), so a crash mid-migration leaves the old store
   intact and the next open simply re-migrates from scratch. Migration
   errors keep their original `Error` variant — no `Error::storage`
   re-wrap and no doubled `storage error:` prefix — with the offending
   version named where the loader knows it.

5. **WAL compatibility story.** `MDBWAL01` is the de-facto WAL version 1
   and, today, the only readable magic, so WAL migration can never trigger
   and there is no separate WAL detection pass: one
   `version::wal::is_readable` classification backs both `scan_seg` and
   the native-ownership admission. Any future `RedoOp` shape change bumps
   the segment magic **and** ships a legacy decoder for the previous magic,
 instead of failing loudly with no path. When [ADR-0120](./0120-ontology-document-persistence.md) lands,
   `MDBWAL02` becomes the first second WAL magic and WAL legacy-magic
   detection moves inside `Wal::open`, which already reads every segment's
   magic: it reports legacy segments and forces a fresh current-magic
   segment **even when the WAL tail is empty** (rotation alone is a no-op
   on an empty tail, so without this rule a legacy-magic store would keep
   appending under the old magic). This changes nothing for older
   binaries: they keep failing at an unknown segment magic before
 modifying data, exactly as [ADR-0120](./0120-ontology-document-persistence.md) specifies. This decision supersedes
 [ADR-0059](./0059-wal-redo-typed-payloads.md) Decision 4's pre-1.0 no-compatibility rationale; [ADR-0059](./0059-wal-redo-typed-payloads.md)'s
 loud-decode-failure rule (its Decision 3, extended by [ADR-0114](./0114-wal-recovery-rejects-undecodable-records.md)) still
   applies to records a binary is expected to understand.

6. **Bundle container stays frozen, gated only by `read_super`.** A `.cl`
   store migrates the payload files inside the container (manifest,
   shards, WAL segments) under the rules above; the container format
 itself — magic `MDBCLB01`, superblock `VERSION = 1` ([ADR-0074](./0074-single-file-bundle-vfs.md)) — is
   bumped only for a true container layout change, never for a payload
   change. The container version is gated only by `read_super` (reached
   via `parse_uri` → `BundleVfs::open_or_create`), which does full A/B
   superblock selection with checksums; there is no second, weaker
   superblock parser for version detection.

7. **Edge cases.**
   - `:memory:` is a no-op: no durable state, nothing to detect or migrate.
   - A corrupt or partial old store fails loudly, with the offending version
     marker (or its absence) named in the error; a truncated v2 manifest or a
     half-written legacy segment is corruption, not a migration input.
   - A store written by a **newer** CypherLite is still rejected at the
 [ADR-0114](./0114-wal-recovery-rejects-undecodable-records.md) gate with the unsupported version in the error; the golden
     newer-version-rejected test is kept.
 - A read-only open ([ADR-0136](./0136-read-only-open.md)) serves a migratable store **through the
     legacy reader without rewriting it**; migration is a property of a
     writable open.

8. **The migration report is a `pub(crate)` value; no log line yet.** The
   report is a plain value whose semantics are "subsystem X was at version
   N, now current"; an empty report means no migration happened (it is not
   an `Option<MigrationReport>`). The report type and its return channel
   stay internal to `cypherlite-storage`, keeping them out of the
   public-API documentation rules. No log or trace line is emitted on
   open: `cypherlite-storage` has no logging facility today, and adding
   one (`log`/`tracing`) is a heavy-dependency decision that needs its own
   ADR under AGENTS.md rule R6. Until then the report is observable only
   through `pub(crate)` test hooks.

9. **Contract tests.** Golden fixture stores per supported version
 (extending the [ADR-0114](./0114-wal-recovery-rejects-undecodable-records.md) `testdata/stores/` set) plus per-version legacy
   reader unit tests — the reader tests are the compatibility proof, since
   there is no hop chain to test; the newer-version-rejected test stays. A
   fixture opens, migrates, serves queries, and reopens at
   `CURRENT_VERSION` byte-identical to a natively written store.

## Alternatives considered

- **In-place / per-record format rewriting.** Rejected: snapshots are
  rewritten wholesale at every checkpoint anyway, so load-legacy +
  checkpoint-current is the cheapest correct transform; in-place rewriting
  adds partial-state hazards that manifest-last publication already solves
  for free.
- **A root-level `store_version` sidecar file.** Rejected: it duplicates
  markers that already exist per subsystem, can disagree with them, and adds
 one more file to keep crash-consistent under [ADR-0089](./0089-immutable-checkpoint-shard-publication.md) publication rules.
- **Refuse old stores; ship an external migration tool or CLI command.**
  Rejected: hands-off migration is the product goal, and an embedded engine
  has no operator to run a tool; the golden stores prove load + checkpoint
  is sufficient.
- **One universal reader that natively understands every old format forever,
  with no migration step.** Rejected: it keeps every legacy decode path on
  the hot read path permanently; the reader-per-version model keeps each
  legacy reader isolated, testable, and removable when
  `MIN_READABLE_VERSION` advances.
- **A hop table: migration as a chain of single-hop pure functions.**
  Rejected: this design was implemented and audited out. The audit found
  the chain was ceremony — a hop was two integers of bookkeeping, no hop
  ever touched data, and the real legacy dispatch already lived in the
  codec loaders (`manifest.format_version <= 2` in the snapshot loader,
  `match value_encoding` in the JSON codec). The migration report was
  invented from the table rather than from what happened, every
  multi-hop error branch was unreachable, and a u64 hop model does not
  fit magic-bytes subsystems (WAL, bundle). Nor has the table a
 foreseeable user: [ADR-0120](./0120-ontology-document-persistence.md)'s future formats are also legacy-reader +
  current-writer, never a function over decoded state. One legacy reader
  per readable version plus the current writer is strictly simpler, and
  per-version reader tests are the chain proof.
- **Give the JSON envelope a real `format_version` field.** Rejected:
  `value_encoding` already is the JSON codec's version dimension, a second
  marker could disagree with it, and older binaries ignore unknown JSON
  fields, so the new field would not protect them anyway.
- **Public `MigrationReport` type or an event callback.** Rejected: a public
  type triggers the public-surface documentation rules (spec, description,
  examples) for a purely informational struct; a `pub(crate)` report value
  covers observability until `cypherlite-storage` has a logging facility
  (its own ADR under AGENTS.md rule R6).
- **Gate migration behind an opt-in flag.** Rejected: hands-off is the
  contract; detection is cheap marker reads and the unconditional run is a
  no-op on current stores, so a flag only adds a way to get stuck on an old
  format.
- **Rewrite WAL records in place to the new shape.** Rejected: WAL segments
 are append-only and ownership-checked ([ADR-0090](./0090-native-reset-role-and-durability.md)); rotate-to-a-fresh-
  segment reuses existing rotation machinery and keeps legacy segments
  immutable until recycled.
- **Keep [ADR-0059](./0059-wal-redo-typed-payloads.md)'s loud-failure stance as the permanent post-1.0 rule.**
 Rejected: [ADR-0059](./0059-wal-redo-typed-payloads.md)'s own Consequences declare it the pre-1.0 exception,
  and with released stores in the field a new `RedoOp` shape must offer a
  migration path, not just a loud failure.

## Consequences

- `open_store` gains a migrate step on its single open flow; the first open
  of an old store pays for a full checkpoint and WAL rotation on the handles
  already open, never a second load. Subsequent opens are at current format.
- Detection adds no I/O: the loaders report the marker they already decoded
  and one registry classification backs every gate.
- Migration is forward-only: once a store is migrated, older binaries fail
  at its version markers. Users who must keep a store readable by an old
  binary must not open it with a new one.
- The registry becomes the single review point for version bumps; every
  future format ADR states its registry change explicitly, including which
  legacy reader decodes the displaced version.
- A mid-migration crash is safe by construction (manifest-last tmp/rename);
  the cost is that migration restarts from the original store on next open.
- When a read-only open mode is added, it must route through legacy readers
  and must not trigger migration.
- [ADR-0120](./0120-ontology-document-persistence.md)'s implementation slots into this contract: `MDBWAL02` becomes the
  first second WAL magic with `MDBWAL01` kept readable, `Wal::open` gains
  legacy-magic detection that forces a fresh current-magic segment even on
  an empty tail, and its `format_version` 4 / `value_encoding` 3 become
  readable registry entries.
- Emitting a log/trace line on migration waits on a logging facility for
  `cypherlite-storage`, which is its own ADR under AGENTS.md rule R6.
- [ADR-0059](./0059-wal-redo-typed-payloads.md) gains a Later references note naming this ADR as the post-1.0
  compatibility contract that supersedes its Decision 4 rationale.
