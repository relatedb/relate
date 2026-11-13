# ADR-0450: `ConstraintDef` carries the enforced label — one schema identity for every constraint kind

Status: Proposed
Claim: 5095ba8b3b8d9ee462f857cf613b61646a57e8ae

## Context

ADR-0172's constraint catalog rejected a second `CREATE CONSTRAINT` whose
schema rule equalled an existing one (`EquivalentSchemaRuleAlreadyExists`).
The rule was a hand-rolled four-field comparison (entity kind, label,
property set, kind) in `Catalog::insert_constraint`, and it deliberately
skipped one kind family:

```rust
// A label/endpoint-label existence kind's identity includes its
// enforced label, which the catalog def does not carry (it lives
// in the theater's derived-constraint index) ...
let identity_includes_enforced_label = kind.is_enforced_label_kind();
if !identity_includes_enforced_label && ... { reject }
```

The skip was correct as a *policy*: a label-existence constraint's identity
is not its constrained label and kind alone — the constraint grammar's
`REQUIRE n:Q` and `REQUIRE n:R` on the same `:P` are two rules, and the
implied-label multi-parent element type `(:A => :B&C)` derives exactly
such a pair. The
skip was wrong as a *mechanism*: it was a kind-shaped bypass of a comparison
that lacked one of the rule's fields, so the catalog and the executor
disagreed. `catalog_create_constraint` already compared property type through
`ConstraintDef::schema_equivalent`, while `insert_constraint` hand-rolled the
same comparison without property type and with the kind bypass — two
spellings of one identity rule, and a durable row that could not state its
own identity.

The enforced label already exists end to end except on the durable catalog
row: the parser AST carries it (`CreateConstraintClause::enforced_label`),
the model's `Constraint` carries it (`enforced_label`, `serde(default)`),
and the theater's derived-constraint index carries it. Only
`ConstraintDef` and the `RedoOp::CreateConstraint` row that feeds it were
missing it, forcing the theater lookup in `sync_derived_constraints`
(`enforced_of`) and the catalog's kind bypass.

## Decision

`ConstraintDef` gains `enforced_label: Option<String>`
(`#[serde(default, skip_serializing_if = "Option::is_none")]`), and
`RedoOp::CreateConstraint` gains the same field. It is populated where the
constraint is authored: the `CREATE CONSTRAINT` path takes it from the
parser clause, and the graph-type derivation path takes it from the model
`Constraint`. A row of a non-enforced-label kind leaves it `None`, and
`skip_serializing_if` keeps that row's serialized bytes identical to the
pre-field shape.

`ConstraintDef::schema_equivalent` becomes the one six-field identity rule —
entity kind, label, property set, kind, property type, enforced label —
taking the candidate `ConstraintDef` itself. `Catalog::insert_constraint`
uses it and the `is_enforced_label_kind` bypass deletes itself. A
`ConstraintDef::schema_probe` constructor lets a caller holding the fields
(not a stored row) build the candidate without inventing a name.

**Legacy rows.** A catalog row written before this field decodes it as
`None`. For an enforced-label kind that means the identity is unknown, so
such a row takes the pre-field kind-scoped policy: it is never declared
equivalent to anything, which keeps a legacy multi-parent graph type's two
same-schema existence rows both loadable. `sync_derived_constraints` fills a
legacy row's label from the theater's derived index (its historical source)
before comparing, so the derive-and-skip behavior on an upgraded store is
unchanged.

The durable shape is additive: an older binary reading a row with the new
key ignores it (serde's default), and a newer binary reading an older row
defaults it to `None`.

## Alternatives considered

- **Keep the kind gate and the four-field comparison.** Rejected: it is a
  kind-shaped bypass around a missing field, it cannot reject two *current*
  rows that share an enforced label, and it keeps two spellings of the
  identity rule (the executor's `schema_equivalent` call and the catalog's
  inline comparison).
- **Keep a separate name→enforced-label map in the catalog (or keep
  consulting the theater).** Rejected: parallel state over one fact that the
  durable row already has to carry; a checkpoint/restore or a WAL replay
  would have to rebuild the map from the same rows anyway, and the theater is
  not available when the catalog row is inserted at preflight.
- **Compare `None == None` as equivalent.** Rejected: a legacy store that
  holds two same-schema multi-parent existence rows would reject the second
  on load and fail to open. Unknown identity must not be declared equal.
- **Recompute the enforced label from the kind.** Rejected: for
  `nodeLabelExistence` the enforced label is the *required* label (`n:Q`),
  not the constrained label — it is not derivable from the def's
  `label`/`kind`.
- **No ADR (treat it as a review fix).** Rejected: the field is part of the
  durable WAL/checkpoint row shape; the repo's decision log owns durable
  format changes.

## Consequences

- One identity rule (`schema_equivalent`) serves the catalog's durable gate
  and the executor's pre-create check; the catalog's inline comparison and
  the `is_enforced_label_kind` bypass are gone. The catalog now also compares
  property type, matching the executor's check. One view
  (`ExecGraph::effective_constraints`) resolves the legacy row before every
  schema comparison.
- Enforcement keeps the theater as its single label source: the validated
  definition carries the resolved label as the one input
  (`ConstraintCheck::new`), but `validate_slice` resolves it from the
  theater's derived index alone, which is the legacy-unenforced signal — a
  store whose theater holds no record for a row stays unenforced.
- A current store rejects a duplicate enforced-label rule and admits
  distinct ones; a legacy store keeps loading and keeps its derive-sync
  behavior.
- Non-enforced rows keep their pre-field checkpoint bytes; enforced rows gain
  one key. Rows are cross-readable in both directions.
- `RedoOp::CreateConstraint` and the IR `Clause::CreateConstraint` carry the
  field, so a future IR construction path cannot silently drop the identity.
