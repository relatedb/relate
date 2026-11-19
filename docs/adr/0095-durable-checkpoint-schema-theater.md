---
type: adr
area: engine
date: 2026-09-24
deciders: CypherLite maintainers
name: 0095-durable-checkpoint-schema-theater
status: proposed
tags:
  - catalog
  - checkpoint
title: Persist admitted schema theater with checkpoint snapshots
REFERENCES:
  - "[[adr/0022-id-window-checkpoint-shards]]"
  - "[[adr/0050-snapshot-codecs]]"
  - "[[adr/0094-preflight-wal-acceptance]]"
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
---

# 0095. Persist admitted schema theater with checkpoint snapshots

## Context

[ADR-0094](./0094-preflight-wal-acceptance.md) admits a single snapshot of named schema and SHOW theater metadata.
WAL replay restores both, but [ADR-0022](./0022-id-window-checkpoint-shards.md)'s Parquet v3 manifest records only
base catalog definitions and [ADR-0050](./0050-snapshot-codecs.md)'s GraphData JSON writes no derived
constraint metadata. After a checkpoint recycles the WAL, a standalone
enforced-label constraint remains named but loses its `enforcedLabel`; writes
that violate the constraint then succeed. Named graph types, databases and
index details similarly lose their SHOW state. A snapshot cannot preserve
less schema than the WAL prefix it replaces.

## Decision

Persist a typed checkpoint theater image alongside the existing named catalog:
in the bounded Parquet v3 `manifest.json` and in JSON `graph.json` (not the
JSON envelope). It contains index details and options, database declarations,
named and current graph types, and derived constraints including exact
enforced labels. Capture it from the committed graph at the checkpoint cut;
restore it before publishing the recovered graph or replaying later WAL.
Reject malformed typed data and inconsistent named enforced-label constraints
on load rather than silently dropping enforcement. When a typed image is present,
every named node or relationship label-existence constraint must have a
same-named derived entry with matching schema fields and a nonempty enforced
label, except for the exact legacy-unknown definitions described below. An
unmarked partial image is corrupt even if every entry it contains is valid.
The Parquet manifest retains its 16 MiB serialized metadata cap with bounded
serialization; an oversized image fails checkpoint before manifest publication
or WAL recycling. Snapshot files continue to be selected and published under
the existing codec checkpoint choreography. Validate every proposed typed
image against the named catalog **before** writing any Parquet shard,
pull-identity map, JSON graph, or manifest. A direct Graph with an incomplete
new named label-existence constraint fails checkpoint with a typed storage
error and leaves the previous checkpoint and WAL intact.

The theater field is optional on read: older manifests and GraphData imports
without it retain their historical defaults. JSON's existing `graph_type`
field remains readable for legacy imports; a new typed theater image takes
precedence when present. Preserve existing JSON constraint `enforced_label`
metadata on legacy imports as typed derived constraints when supplied. No
snapshot field changes query-time validation or the WAL representation.
Track the exact named label-existence definitions whose enforcement was already
unknown in a theater-absent legacy snapshot or legacy WAL replay. Do not guess
their `enforcedLabel`. A typed checkpoint image carries a narrowly scoped
`legacy_unknown_constraints` map of those names to their full named catalog
definitions, alongside all known theater (including newly committed databases,
graph types, index details, and fully known enforced labels). On load and before
write, each marked definition must match the current named catalog exactly,
must require an enforced label, and must lack a derived entry. A missing or
incomplete derived entry for any unmarked named label-existence constraint is
corruption; a marker for an unrelated, changed, or fully known definition is
also corruption. Dropping or recreating a constraint with complete derived
metadata removes its legacy marker. Theater-absent legacy files and typed images
without the marker remain readable, with the latter requiring complete derived
entries. Checkpoint and recovery preserve the existing 16 MiB Parquet manifest
cap, including the marker map. A new incomplete constraint never acquires
legacy provenance merely by being committed after a legacy import.

## Alternatives considered

- **Regenerate all theater from base catalog definitions.** Rejected: the
  catalog omits enforced labels, index kinds/options, named graph types and
  database declarations; regeneration cannot reproduce the committed state.
- **Keep schema WAL forever.** Rejected: checkpoint recycling must not depend
  on retaining an unbounded historical schema log.
- **Add a separate mutable schema sidecar.** Rejected: a sidecar without an
  immutable manifest selector can get out of step with the checkpoint LSN and
  its published entity image.
- **Ignore unknown or malformed theater entries on open.** Rejected: silently
  discarding a required label changes constraint enforcement after recovery.
- **Omit the entire theater for an unresolved legacy constraint.** Rejected:
  WAL recycling then discards unrelated, newly committed theater such as a
  database declaration.
- **Write an incomplete typed theater without provenance.** Rejected: it makes
  a newly incomplete constraint indistinguishable from a legacy unknown.

## Consequences

- Parquet v3 manifests and JSON GraphData grow with committed schema, within
  the existing Parquet metadata bound; a too-large Parquet schema stops the
  checkpoint without advancing its WAL cut.
- Older files with no theater field still open and can checkpoint/reopen with
  their known legacy semantics. Legacy-unknown definitions survive as narrowly
  validated markers while all known SHOW output and enforced-label validation
  survive WAL recycling; incomplete new candidates fail before checkpoint files
  are written.
- [ADR-0022](./0022-id-window-checkpoint-shards.md)'s v3 manifest gains optional schema metadata; [ADR-0050](./0050-snapshot-codecs.md)'s GraphData
  gains the equivalent optional typed field without changing the JSON envelope.

## Later references

[ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) keeps only the fixed namespaces' entries in the theater `databases`
map. Entries for other names, from earlier `CREATE DATABASE` calls, are
ignored on load and replay and left out of the next checkpoint image. The
theater field and its format are otherwise unchanged.
