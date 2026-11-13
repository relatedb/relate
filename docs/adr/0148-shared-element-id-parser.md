# 0148. One element-id shape parser

- **Status**: Proposed
- **Date**: 2026-09-28
- **Deciders**: CypherLite maintainers

## Context

The element-id shape `<kind>:<segment>:<n|r><id>` (ADR-0116 §7) was parsed
twice: `USE graph.byElementId(...)` resolution in
`cypherlite-planner/src/use_target.rs` split the id and mapped the middle
segment, and `db.nameFromElementId` in
`cypherlite-executor/src/eval/function.rs` split it again with its own
extra checks (kind prefix `4`/`5`, non-empty entity segment). Two
hand-rolled splits of one wire format drift. Flagged in review of the
fixed-namespaces work.

## Decision

`cypherlite_core::ElementIdRef<'a>` (next to `Namespace`) is the single
parser of the element-id shape: `ElementIdRef::parse(id)` splits exactly
three colon-separated segments into borrowed `kind` / `segment` / `id`
slices (`None` otherwise), and `namespace()` maps the middle segment to
its `Namespace` via the existing `Namespace::from_element_id_segment`.
Both former call sites delegate. Behavior and error texts are unchanged:
`db.nameFromElementId` keeps its own kind-prefix/empty-entity checks on
top of the shared split and still does not validate the entity segment's
contents (a frozen eval scenario uses `4:cypherlite:999999`); the 42NA7
text of `USE graph.byElementId` is byte-identical (fixture-pinned in
`testdata/ce-errors/neo4j-ce.json`).

## Alternatives considered

- **Two parsers with a shared segment-mapping helper only.** Rejected: the
  duplicated part was the three-part split itself, not the segment map
  (`Namespace::from_element_id_segment` was already shared); sharing only
  the map leaves the shape parsing to drift.
- **A validation-rich parser that also checks kind and entity segments.**
  Rejected: the two callers want different strictness (`use_target`
  resolves by segment alone; `nameFromElementId` checks kind and entity
  presence but deliberately not entity contents); one strict parser would
  have to grow mode flags, recreating the split it was meant to delete.

## Consequences

- `cypherlite-core` exports `ElementIdRef`; `use_target.rs` and
  `eval/function.rs` delegate to it.

## Later references

- ADR-0147: the typed `QueryResult.namespace`, split out of this file's
  original draft (one decision per file).
