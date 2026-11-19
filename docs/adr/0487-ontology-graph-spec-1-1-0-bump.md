---
type: adr
area: engine
claim: 247bd141ff7cf03e5a7cf75eda2665ee4e52a8a3
name: 0487-ontology-graph-spec-1-1-0-bump
status: accepted
tags:
  - ontology
title: Bump the ontology document gate to Ontology Graph Spec 1.1.0
REFERENCES:
  - "[[adr/0226-ontology-mappings]]"
  - "[[adr/0227-ontology-document-format]]"
  - "[[adr/0364-ontology-spec-main-gate]]"
SUPERSEDES:
  - "[[adr/0364-ontology-spec-main-gate]]"
---

# ADR-0487: Bump the ontology document gate to Ontology Graph Spec 1.1.0

## Context

[ADR-0364](./0364-ontology-spec-main-gate.md) pinned the Ontology Graph Spec document to `neo4j/graph-spec`
main @ `066515593ae59317d758bcf2cef54195e74bde39` (2026-10-19), whose
`$id` is `https://neo4j.com/docs/ontology-graph-spec/1.0.0/schema.json`.
Its §1 gates `db.ontology.set` on that exact URI (CypherLite policy:
main's `schema` is an unconstrained string), its §2 vendors that schema
and hand-rolls validation against exactly it, and its §5 re-bases the
[ADR-0226](./0226-ontology-mappings.md) §2 converter on the main shapes.

Neo4j has now published **Ontology Graph Spec 1.1.0** as the tag
`release-spec/v1.1.0` (`neo4j/graph-spec` @
`5ff2c20aa7e428e7c563b17f573157293959a009`; current main @
`1656fb506543f78cd2f73a7897bd8437632de509` serves a byte-identical
schema). Its `$id` is
`https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json`. A diff
against the vendored 1.0.0 pin shows seven structural changes; everything
else — the 39-token `Neo4jType` enum, the constraint-type enum, the
index types and their option payloads, the mapping union,
`RelationshipTarget`, every required list and every `additionalProperties`
closure — is unchanged:

1. Node entries lost the nullable display `name` member.
2. Relationship entries lost the nullable display `name` member.
3. Property entries regained `aliases` (an array of strings) — the member
 main 1.0.0 removed and [ADR-0364](./0364-ontology-spec-main-gate.md) §2 rejects as unknown.
4. Node constraints lost `label`, so `NodeConstraint` and
   `RelationshipConstraint` are now the same closed shape.
5. Document extensions renamed `tables` → `data_source_tables` and
   `mappings` → `data_source_mappings` (same payloads, same inner key
   names).
6. Table columns (`TableColumn`) gained `nullable`, a plain optional
   boolean — whether the column can hold null values.
7. The `$id` version moved to 1.1.0 (plus wording-only description edits:
   "internalised" → "decoded", and the surviving property/constraint/index
   `name` descriptions now describe database names rather than pretty-form
   names).

The two dialects are mutually incompatible on closed shapes: a 1.0.0
document carrying a node or relationship `name`, a constraint `label`, or
`tables`/`mappings` fails 1.1.0's closed shapes, and a 1.1.0 document
carrying property `aliases` or `data_source_*` fails the 1.0.0 shapes.
CypherLite must again choose between a compatibility shim and a hard
cutover, and update the gate, the vendored schema, the validator, the
converter and the fixtures.

## Decision

### 1. One gate URI, hard cutover again

- The only accepted value of the root `schema` field becomes
  `https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json` — the
  release tag's `$id`, checked before structural validation as before.
  `PINNED_SCHEMA_URI` becomes the new URI and
  `MAX_SUPPORTED_SPEC_VERSION` becomes `1.1.0`; the
  `UnsupportedSpecVersion` message interpolates both, so it cannot drift.
  The retired 1.0.0 main URI, the older draft URI, and any other string
  fail at the gate.
- Hard cutover, for the same reasons as [ADR-0364](./0364-ontology-spec-main-gate.md) §1: no dual gate, no URI
  alias, no rewrite, no automatic migration. A 1.0.0-main document fails
  `set` at the gate; a 1.1.0-URI document carrying a 1.0.0-only member
  fails structural validation on that member. Resubmitting a 1.1.0-shape
  document is the migration.
- Stored documents are never re-validated ([ADR-0364](./0364-ontology-spec-main-gate.md) §4, unchanged): `get`
  keeps returning a stored 1.0.0 document's bytes verbatim, format
  conversion still runs no gate, and `apply` refuses it with the gate's
  error until it is replaced.

### 2. Validation follows the 1.1.0 shapes

The vendored schema file is replaced in place with the tag's schema and
the hand-rolled validator checks exactly that document. The six deltas
land as:

| Construct | Change |
|---|---|
| Node entry | `name` is an unknown member (the closed set loses it). |
| Relationship entry | `name` is an unknown member. |
| Property entry | `aliases` (string array) is accepted and carried, descriptive like node and relationship `aliases`; `mustExist` stays unknown. |
| Constraint objects (both owners) | one closed shape: `type`, `properties`, `extensions`, nullable `name`. `label` is an unknown member on node constraints too; the validator's node-vs-relationship constraint distinction collapses with it. |
| Document extensions | `data_source_tables` and `data_source_mappings`, deeply validated exactly as `tables`/`mappings` were; the old key spellings are unknown members. |
| Table columns | gained `nullable` (a plain optional boolean, read with the shared boolean reader); every other `TableColumn` member is unchanged. |
| Root, labels, endpoints, indexes, display, mappings | unchanged. |

### 3. Converter on the new shapes ([ADR-0226](./0226-ontology-mappings.md) §2 re-based)

[ADR-0364](./0364-ontology-spec-main-gate.md) §5 survives except where it reads the removed members:

- The §5.1/§5.4 node-constraint `label` rules die: a constraint always
  compiles against its owning element, and the label check is deleted
  with the member.
- Node and relationship display `name`s no longer exist to carry; the
  property, constraint and index `name`s remain descriptive (a constraint
  object's `name` still rides the graph-type constraint name).
- Property `aliases` is descriptive and compiles to nothing, exactly like
  node and relationship `aliases`.
- The token mapping (§5.2), the shorthand flags and the
  object-replaces-flag rule, the `PROPERTY_TYPE` loud failure, endpoint
  resolution, and every other §5.3/§5.4 loud mapping error stand
  unchanged.

### 4. Example fixture and provenance

`schema/example.json` is reworked to the 1.1.0 shape — same coverage
contract as [ADR-0364](./0364-ontology-spec-main-gate.md) §6 (every `Neo4jType` token, all four constraint
types, the nullable/empty forms, every typed extension member) — with the
node/relationship `name`s and constraint `label`s removed, property
`aliases` added, and the document extension keys renamed.
`schema/README.md` records the new pin (`release-spec/v1.1.0` @
`5ff2c20aa7e428e7c563b17f573157293959a009`, byte-identical to main @
`1656fb506543f78cd2f73a7897bd8437632de509`) and keeps the upstream notes
(no root-level upstream example; the Go testdata file's invalid endpoint
`property` members).

## Alternatives considered

- **Accept both the 1.0.0 and 1.1.0 URIs (dual dialect).** Rejected: the
  shapes are mutually incompatible on closed members, so a dual gate
  either maintains two validators or accepts a union no dialect defines,
  and it obscures which dialect was accepted. Same reasoning as
 [ADR-0364](./0364-ontology-spec-main-gate.md)'s rejection; the product is still pre-1.0.
- **Version-prefix matching (`…/1.x/…`).** Rejected: the exact URI is the
  only discriminator — the draft and main both called themselves 1.0.0
 and were different dialects. [ADR-0227](./0227-ontology-document-format.md) §7 and [ADR-0364](./0364-ontology-spec-main-gate.md) §1 already
  rejected lenient matching.
- **Validate the 1.0.0 shapes behind the 1.1.0 gate (accept the union).**
  Rejected: the validator mirrors the vendored schema exactly
 ([ADR-0364](./0364-ontology-spec-main-gate.md) §2); a union would pass documents the published schema
  rejects and reject documents it accepts.
- **Migrate stored 1.0.0 documents at open or `get`.** Rejected: the
 store keeps bytes verbatim ([ADR-0227](./0227-ontology-document-format.md) §2); migration rewrites them, and
  an old binary's read path must not become the migration engine
 ([ADR-0364](./0364-ontology-spec-main-gate.md) §4).
- **Keep the constraint `label` and display-`name` rules in the converter
  as dead code.** Rejected: rules reading members that can no longer
  parse are drift, and the no-parallel-state rule forbids carrying a
  distinction (node-vs-relationship constraint shapes) the schema no
  longer makes.

## Consequences

- A 1.0.0-main document fails `set` at the gate with the version error
  naming 1.1.0; a 1.1.0 document with a 1.0.0-only member fails
  structurally on that member. Stored 1.0.0 documents stay readable via
  `get`, convertible across formats, and refused at `apply` until
  resubmitted.
- The stored-document format is unchanged (no WAL/snapshot change); a
 pre-bump binary's fail-closed open behavior repeats [ADR-0364](./0364-ontology-spec-main-gate.md) §4's story
  verbatim.
- The typed model loses `NodeType::name`, `RelationshipType::name` and
  `Constraint::label`, and `Property` gains `aliases`; the validator's
  `ConstraintOwner` distinction is deleted with the constraint `label`.
  The `Validation` error text interpolates `MAX_SUPPORTED_SPEC_VERSION`
  instead of hardcoding the version, so the message cannot drift from the
  gate constants.
- Tests: the gate suite gains the retired-1.0.0-URI rejection and
  old-member structural rejections; the clause suite flips property
  `aliases` to acceptance and constraint `label` to unknown-member on
  both owners; the extension suites rename the document keys; the
  fixture suite pins the vendored 1.1.0 shapes. Docs surfaces that quote
  the gate (rust spec, ontology-procedures reference, cypher-syntax
  reference, CLI skill operations reference) are updated in this change.
- [ADR-0364](./0364-ontology-spec-main-gate.md)'s §1 gate URI, §2 accepted shapes, §5.1/§5.4 constraint-label
  rules and §6 example are superseded by this ADR (noted there);
 [ADR-0226](./0226-ontology-mappings.md) §2's converter rules are re-based on the 1.1.0 shapes (noted
 there). [ADR-0364](./0364-ontology-spec-main-gate.md)'s §3/§4 rulings (main-valid-but-not-convertible, and
  set-time-only policy) stand.
- Supporting a later spec version remains a new ADR, as [ADR-0364](./0364-ontology-spec-main-gate.md) §1
  already prescribes.
