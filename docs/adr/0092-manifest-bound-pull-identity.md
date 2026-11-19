---
type: adr
area: engine
date: 2026-09-23
deciders: CypherLite maintainers
name: 0092-manifest-bound-pull-identity
status: proposed
tags:
  - load
  - storage
title: Publish pull identity with the selected snapshot manifest
REFERENCES:
  - "[[adr/0074-single-file-bundle-vfs]]"
  - "[[adr/0080-cli-pull-command]]"
  - "[[adr/0086-bounded-persistence-input]]"
  - "[[adr/0089-immutable-checkpoint-shard-publication]]"
  - "[[adr/0091-bundle-generation-page-quarantine]]"
  - "[[adr/0098-safe-atomic-replacement]]"
  - "[[adr/0106-json-snapshot-pull-identity]]"
  - "[[adr/0158-streaming-single-process-pull]]"
---

# 0092. Publish pull identity with the selected snapshot manifest

## Context

[ADR-0080](./0080-cli-pull-command.md) defines pull mappings as WAL ops in the same commit as graph changes
and snapshots them to `pull-identity.json`. Its intended "same durability
boundary" does not follow from writing two independently replaced files:
today the v3 manifest is published first and the mutable identity file is
written afterward. A crash or a failed second write can pair a newly selected
graph with an old map; a later checkpoint can pair an old manifest with a
new map. [ADR-0089](./0089-immutable-checkpoint-shard-publication.md) makes Parquet parts immutable and manifest-selected, but
the pull map remains outside that publication set. [ADR-0074](./0074-single-file-bundle-vfs.md) requires a `.cl`
store to remain one host file.

## Decision

1. **Manifest selects one immutable map.** Extend the existing v3 manifest
   with an optional `pull_identity_path` field. New checkpoints always write
   one complete pull-identity JSON object, including `[]` for an empty map,
   under an exclusive, generation-unique virtual path such as
   `pull-identity/{128-bit-random-hex}.json`. Never replace an existing map
   path. Validate that a selector has precisely this confined spelling,
 resolves to a regular non-symlink object and obeys [ADR-0086](./0086-bounded-persistence-input.md)'s 16 MiB read
   envelope. The referenced map must exist and decode: a missing, malformed
   or oversized selected map is a storage error, never an empty mapping and
   never a reason to fall back to a different generation.

2. **One publication point.** Serialize, exclusively create, write, sync and
   durably name the new map before publishing `manifest.json`. Do the same
 for every new Parquet shard per [ADR-0089](./0089-immutable-checkpoint-shard-publication.md). Put the selector and all shard
   references in the manifest; its durable atomic replacement is the *only*
   publication decision. Load the selected snapshot and selected map before
   streaming WAL replay, which still applies pull identity ops from their
 original graph commits ([ADR-0080](./0080-cli-pull-command.md)). Do not rewrite the legacy
   `pull-identity.json` after manifest publication. Before a successful
   manifest barrier, keep the old manifest, its selected map/parts, and its
   required committed WAL intact. If rename or sync reports ambiguous
   publication, inspect the readable manifest and retain all possibly
   referenced objects; do not recycle WAL on failure. Bundle's virtual
 objects obey [ADR-0091](./0091-bundle-generation-page-quarantine.md)'s selected-generation page barrier, Memory orders
 virtual publication, and Native follows [ADR-0098](./0098-safe-atomic-replacement.md) (including its explicitly
   weaker Windows power-loss namespace guarantee).

3. **Legacy compatibility and cleanup.** A v3 manifest *without* the field
   continues loading `pull-identity.json` if present, or an empty map if
   absent; v1/v2 snapshot behavior remains supported. This fallback applies
   **only** when the selector is absent. A checkpoint of a legacy store writes
   a new immutable map and selects it with the next manifest; no eager
   migration on open and no format-version bump. Reclaim only map objects
   proven unreferenced by an authoritative, validated manifest; defer cleanup
   after ambiguous publication or on Native Windows where an older manifest
   may return after power loss. Keep the legacy file while a legacy manifest
 may still be selected. Apply [ADR-0086](./0086-bounded-persistence-input.md)'s bounds to both legacy and new map
   reads and writer output.

## Alternatives considered

- **Continue writing `pull-identity.json` after the manifest.** Rejected:
  successful manifest publication followed by a map-write crash can mix
  different checkpoint generations.
- **Write the mutable identity file first.** Rejected: a crash before manifest
  replacement now pairs the old graph with the new map; reversing the two
  writes cannot make them atomic.
- **Use a missing selected map as an empty map or fall back to the legacy
  sidecar.** Rejected: this hides corruption and silently loses idempotent
  pull mappings for an otherwise committed checkpoint.
- **Put all mappings inline in the manifest or add a new host `.cl` sidecar.**
  Rejected: inline mappings inflate metadata and its decode envelope;
 a host sidecar violates [ADR-0074](./0074-single-file-bundle-vfs.md)'s one-file container contract.
- **Immediately delete old maps after any rename response.** Rejected:
  publication can fail after the new manifest is visible, and on Windows
  an older manifest may reappear after a power loss.

## Consequences

- A selected v3 manifest always names the graph shards and precisely one map
  from the same checkpoint generation. `cypherlite-load` keeps its
 source-scoped upsert semantics and WAL identity ops from [ADR-0080](./0080-cli-pull-command.md).
- Legacy v3 stores remain readable, including a missing optional sidecar;
  already inconsistent legacy manifest/map pairs cannot be retroactively
  repaired. New map objects use virtual paths inside `.cl`, not host files.
  Tests must cover repeat pull, failures before and after publication, legacy
  fallback, and Native/Memory/Bundle recovery and cleanup.

## Later references

[ADR-0106](./0106-json-snapshot-pull-identity.md) proposes persisting pull identity for the JSON snapshot codec
inline in `graph.json` (`GraphData.pull_identity`) instead of through this
manifest-selected sidecar. The Parquet mechanism above is unchanged.

- [ADR-0158](./0158-streaming-single-process-pull.md) retires persisted pull-identity objects entirely: the map is load-scoped memory; existing objects are inert legacy data (never read; outside storage's sweep).
