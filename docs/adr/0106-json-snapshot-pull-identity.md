---
type: adr
area: engine
date: 2026-09-25
deciders: CypherLite maintainers
name: 0106-json-snapshot-pull-identity
status: proposed
tags:
  - checkpoint
  - load
title: JSON checkpoints carry pull identity inline
REFERENCES:
  - "[[adr/0022-id-window-checkpoint-shards]]"
  - "[[adr/0050-snapshot-codecs]]"
  - "[[adr/0074-single-file-bundle-vfs]]"
  - "[[adr/0080-cli-pull-command]]"
  - "[[adr/0086-bounded-persistence-input]]"
  - "[[adr/0092-manifest-bound-pull-identity]]"
  - "[[adr/0104-cargo-feature-bundles]]"
  - "[[adr/0105-optional-parquet-codec]]"
---

# 0106. JSON checkpoints carry pull identity inline

## Context

`cypherlite pull` ([ADR-0080](./0080-cli-pull-command.md)) is a source-scoped upsert. It keys every pulled
element by `(source, kind, remote elementId)` and records the local id in a
pull-identity map. The map is written as WAL ops in the same commit as the
graph changes and is restored at checkpoint load. [ADR-0092](./0092-manifest-bound-pull-identity.md) binds the map to
the snapshot: the v3 Parquet manifest selects one immutable sidecar object,
`pull-identity/<hex>.json`, through `pull_identity_path`.

Only the Parquet snapshot path implements this. The JSON codec ([ADR-0050](./0050-snapshot-codecs.md))
writes and reads the `GraphData` envelope in `graph.json`, and that envelope
has no pull identity. `pull` checkpoints right after loading, and close
checkpoints again and recycles the WAL. In a JSON store the mappings are
therefore gone after the first pull, and a repeat pull duplicates nodes and
relationships instead of reusing them — the same bug [ADR-0080](./0080-cli-pull-command.md) fixed for
Parquet.

[ADR-0105](./0105-optional-parquet-codec.md) (open, not yet on `main`) puts the Parquet codec behind an additive
`parquet` Cargo feature. It makes `pull` require that feature "for now",
because pull identity is Parquet-only, and names LABS-104 as the follow-up
that removes the coupling so the CLI `agent` bundle ([ADR-0104](./0104-cargo-feature-bundles.md)) can drop
Arrow/Parquet.

The reasons [ADR-0092](./0092-manifest-bound-pull-identity.md) uses a separate sidecar do not carry over to JSON.
Parquet checkpoints are incremental ([ADR-0022](./0022-id-window-checkpoint-shards.md)): a sidecar avoids rewriting
unchanged mappings, and the manifest is the one atomic publication point for
many objects. JSON `dump` rewrites all of `graph.json` on every checkpoint,
and `graph.json` is already the single atomic unit of a JSON snapshot.

## Decision

1. **Inline field.** Add an optional `pull_identity` field to the JSON
   `GraphData` envelope in `graph.json`:

   ```rust
   #[serde(default, skip_serializing_if = "Vec::is_empty")]
   pull_identity: Vec<PullIdentityEntry>,
   ```

   `PullIdentityEntry` is the existing `cypherlite_executor` type
   (`source`, `kind` as lowercase `node`/`rel`, `remote_id`, `local_id`).
   No new wire type and no format-version bump.

2. **Dump.** JSON `dump` writes `Graph::pull_identity_entries()` into the
   field as part of the same full `graph.json` rewrite. An empty map writes
   nothing.

3. **Load.** JSON `load` restores the field with
   `Graph::restore_pull_identity` before WAL replay. A missing field is an
 empty map. WAL `PullIdentityPut` replay is unchanged ([ADR-0080](./0080-cli-pull-command.md)).

4. **Validation.** Every `local_id` is checked with
   `PullIdentityEntry::validate_local_ids` on dump and on load, the same
   shared check the Parquet path uses. Entries are part of `graph.json` and share its existing
   read path; no separate size envelope is added.

5. **Parquet unchanged.** The [ADR-0092](./0092-manifest-bound-pull-identity.md) manifest-selected sidecar, its
   selector validation, legacy fallback and cleanup stay as they are. The
   JSON `manifest.json` envelope is unchanged.

## Alternatives considered

- **One codec-neutral, manifest-selected pull-identity sidecar for both
 codecs** (reuse the [ADR-0092](./0092-manifest-bound-pull-identity.md) machinery in the JSON path). Rejected: JSON is
  a full-rewrite codec, so a sidecar gives no write-amplification benefit.
  It would drag manifest-selected objects, selector validation and orphan
  object GC into the JSON path, and it needs a new selector field in the
  JSON `manifest.json` envelope. That is more publication surface for no
  gain over one atomically replaced `graph.json`.
- **Leave the JSON gap and keep `pull` coupled to the `parquet` feature
 permanently** (make [ADR-0105](./0105-optional-parquet-codec.md)'s "for now" final). Rejected: a repeat pull
  into a JSON store silently duplicates data today, and the coupling keeps
  Arrow/Parquet in the `agent` CLI bundle.
- **Store the entries in the JSON `manifest.json` envelope.** Rejected:
 `manifest.json` is bounded metadata ([ADR-0086](./0086-bounded-persistence-input.md)) and would inflate with
  every pulled element. It is also a second file written separately from
 `graph.json`, which reopens the mixed-generation problem [ADR-0092](./0092-manifest-bound-pull-identity.md) fixed.
- **A mutable `pull-identity.json` next to `graph.json`.** Rejected: two
  independently replaced files cannot be published atomically — the exact
 failure [ADR-0092](./0092-manifest-bound-pull-identity.md) was written to remove.

## Consequences

- **Backward compatible.** A `graph.json` written before this change has
  no field and loads with an empty map, which is today's behavior.
  `GraphData` does not use `deny_unknown_fields`, so an older binary reading
  a new `graph.json` ignores the field and loses the mappings — also
  today's behavior, not a new failure.
- **Zero cost when unused.** A store that never pulled writes no extra
  bytes; its `graph.json` is byte-identical to before.
- **Idempotent pull on JSON.** A repeat pull into a JSON store reuses
  mapped elements instead of duplicating them, matching Parquet.
- **Tests required.** JSON dump/load roundtrip of pull identity on Native
 and Memory VFS and inside a `.cl` bundle ([ADR-0074](./0074-single-file-bundle-vfs.md)); load of an old
  `graph.json` without the field; rejection of an invalid `local_id`; and a
  CLI end-to-end repeat pull against a JSON store that asserts node and
  relationship counts do not grow.
- **Unblocks [ADR-0105](./0105-optional-parquet-codec.md) follow-up.** Once this lands, `pull` no longer needs
  the `parquet` feature and the CLI `agent` bundle can drop Arrow/Parquet.
 That change is recorded in [ADR-0105](./0105-optional-parquet-codec.md) § "Pull does not require Parquet".
- The Parquet snapshot path and [ADR-0092](./0092-manifest-bound-pull-identity.md) are untouched.
