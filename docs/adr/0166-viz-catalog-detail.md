# 0166. `db.schema.visualization()`: populate `indexes`/`constraints` from the catalog

- **Status**: Accepted
- **Date**: 2026-10-05
- **Deciders**: CypherLite maintainers (Linear LABS-172)

## Context

ADR-0153 shipped `db.schema.visualization()` with Neo4j's `{name, indexes,
constraints}` virtual-node property shape, but with `indexes` and
`constraints` as **empty documented placeholders**; "Populate
`indexes`/`constraints` from the catalog" is listed under its Alternatives
considered as rejected, on the grounds that index and constraint
introspection is a separate catalog surface and duplicating it would couple
three renderings of the same facts.

The graph-examples differential suite (ADR-0144) compares cypherlite's
output against a Neo4j reference, and LABS-172 reports the mismatch this
placeholder choice produces: Neo4j's `CALL db.schema.visualization()`
returns each label's virtual node with the catalog detail populated, while
cypherlite returns empty lists. The rejected alternative is the bug being
fixed; this ADR reverses exactly that one ADR-0153 alternative and leaves
everything else in ADR-0153 in force.

The exact Neo4j rendering was verified by a Docker probe of
`neo4j:2025-enterprise` (a `Client` label with a uniqueness constraint, a
single-property index, and a composite index; an `SSN` node-key constraint;
a relationship property-existence constraint on `TRANSFER_TO`; plus data).
Observed `CALL db.schema.visualization()` output:

```
nodes:
(:Client {name: "Client", indexes: ["name", "name,id"], constraints: ["Constraint( id=3, name='client_id_unique', type='NODE PROPERTY UNIQUENESS', schema=(:Client {id}), ownedIndex=2 )"]})
(:SSN {name: "SSN", indexes: [], constraints: ["Constraint( id=7, name='ssn_value_key', type='NODE KEY', schema=(:SSN {value}), ownedIndex=6 )"]})
relationships: [[:TRANSFER_TO {name: "TRANSFER_TO"}]]
```

Facts the probe established:

- `indexes` is a list of strings, **one per index** on that label, each
  string the index's comma-joined **property** list (`"name"`, `"name,id"`)
  — not index names.
- **Constraint-backing indexes are not listed** in `indexes`: the probe's
  `:Client` label carried a uniqueness constraint on `id` whose backing
  index existed (surfacing only as `ownedIndex=2` on the constraint
  string), yet `indexes` rendered `["name", "name,id"]` — no `"id"` entry.
- `constraints` holds one string per **node** constraint on that label:
  `Constraint( id=N, name='<name>', type='<TYPE>', schema=(:<Label> {<props>}), ownedIndex=N )`,
  with the type in Neo4j's **space** style (`'NODE PROPERTY UNIQUENESS'`,
  `'NODE KEY'`), not the underscore style `SHOW CONSTRAINTS` uses.
- The relationship constraint (`txn_amount_exists` on `TRANSFER_TO`)
  appeared **nowhere** — relationship constraints are not represented in
  this procedure.
- `id`/`ownedIndex` are Neo4j-internal schema-rule ids.

CypherLite-side facts:

- The catalog (`cypherlite/cypherlite-catalog/src/lib.rs:9-35`) is
  **name-keyed**: `IndexDef { name, label, property }` (a single property —
  no composite indexes) and
  `ConstraintDef { name, relationship, label, properties, kind, property_type }`,
  held in `BTreeMap<String, _>` maps keyed by name. There are **no
  schema-rule ids and no owned-index ids** to report.
- `ExecGraph` exposes overlay-aware `named_indexes()` and
  `named_constraints()`
  (`cypherlite/cypherlite-executor/src/overlay.rs:365,413`) — the same
  accessors `SHOW INDEXES`/`SHOW CONSTRAINTS` render from.
- `IndexDef.property` may carry a legacy `property#kind` encoding, decoded
  by `split_legacy_property_encoding`
  (`cypherlite/cypherlite-executor/src/exec/catalog_schema.rs:20`).
- Constraint kinds are `"unique"`, `"key"`, `"exists"`, `"propertyType"`,
  `"labelExistence"`/`"nodeLabelExistence"`, `"sourceLabel*"`,
  `"targetLabel*"`; `show_constraints_rows`
  (`cypherlite/cypherlite-executor/src/exec/catalog_show/mod.rs:345-420`)
  maps them to the underscore-style display names.
- The placeholders live in `schema_visualization_rows`
  (`cypherlite/cypherlite-executor/src/exec/procedure_rows/schema.rs:108`,
  empty lists at `:133-134`).

## Decision

`db.schema.visualization()` populates each virtual label node's `indexes`
and `constraints` from the catalog **at call time**, on top of the
procedure's existing scan of stored entities (same scan-time cost model as
the rest of the procedure, ADR-0153). Everything is derived from the
overlay-aware `named_indexes()`/`named_constraints()` accessors, so the
rendering tracks in-flight DDL exactly as `SHOW INDEXES`/`SHOW CONSTRAINTS`
do.

1. **`indexes`: sorted list of decoded property-name strings, excluding
   constraint-backing indexes.** One string per catalog index whose `label`
   matches the virtual node's label, **except** an index whose name equals
   the name of a `unique`/`key` constraint in the catalog — that is the
   backing index `catalog_create_constraint` installs under the
   constraint's own name, and Neo4j lists such indexes only as `ownedIndex`
   on the constraint string (a field cypherlite omits, item 3). Evidence:
   the Docker probe's `:Client` label (uniqueness constraint on `id` with
   backing index `ownedIndex=2`, plus explicit indexes on `(name)` and
   `(name,id)`) rendered `indexes: ["name", "name,id"]` — the backing
   index's `"id"` was absent. This exclusion is specific to
   `db.schema.visualization()`; `SHOW INDEXES` intentionally keeps listing
   backing indexes (Neo4j parity there). The string is the index's property
   after decoding the legacy `property#kind` encoding
   (`split_legacy_property_encoding`). The list is sorted ascending. Because
   the catalog model is single-property (`IndexDef.property` is one
   `String`), no composite `"name,id"` rendering arises; if composite
   indexes are ever added to the catalog, each index's string becomes its
   comma-joined property list, matching Neo4j.
2. **`constraints`: sorted list of rendered `Constraint(...)` strings.**
   One string per catalog constraint with `relationship == false` whose
   `label` matches, sorted by constraint name, rendered as:

   ```
   Constraint( name='<name>', type='<TYPE>', schema=(:<Label> {<props>}) )
   ```

   `<props>` is the constraint's comma-joined property list; a constraint
   with no properties renders `schema=(:<Label>)`. `<TYPE>` is Neo4j's
   **space** style, mapped from the catalog kind:

   | Catalog kind | Rendered type |
   | --- | --- |
   | `unique` | `NODE PROPERTY UNIQUENESS` |
   | `key` | `NODE KEY` |
   | `exists` | `NODE PROPERTY EXISTENCE` |
   | `propertyType` | `NODE PROPERTY TYPE` |
   | `labelExistence` / `nodeLabelExistence` | `NODE LABEL EXISTENCE` |
3. **No `id`/`ownedIndex` fields.** CypherLite's catalog is name-keyed and
   has no schema-rule or owned-index ids; synthesizing fake ones would be
   misleading, so the rendered string omits both fields (and the commas
   that would separate them).
4. **Relationship constraints appear on no virtual node.** Constraints
   with `relationship == true` are skipped entirely — Neo4j parity (the
   probe's `TRANSFER_TO` existence constraint appeared nowhere).
5. **Virtual relationships keep carrying no properties.** ADR-0153 is
   unchanged on that point; Neo4j's `{name: "<TYPE>"}` on virtual
   relationships is a separate concern, out of scope here.

## Alternatives considered

- **Keep the empty documented placeholders (ADR-0153's status quo).**
  Rejected: the differential-suite mismatch against the Neo4j reference is
  the bug being fixed (LABS-172); the placeholders make every
  catalog-bearing graph diverge from the oracle.
- **Synthesize positional ids for `id`/`ownedIndex`.** Rejected: the ids
  would be fake data — stable-looking but meaningless, and misleading to
  any client that treats them as addressable schema-rule handles; they
  still would not match the oracle's ids, so the differential gain is zero.
- **Reuse the `SHOW CONSTRAINTS` underscore type style**
  (`NODE_PROPERTY_UNIQUENESS`, …). Rejected: Neo4j's `Constraint()` string
  uses the space style (`NODE PROPERTY UNIQUENESS`) and the differential
  suite compares against it; matching the underscore style would re-create
  the mismatch this ADR exists to fix.

## Consequences

- ADR-0153's "empty documented placeholders" bullet (Decision, node
  property shape) is **superseded** by this ADR; a **Later references**
  note on ADR-0153 records the reversal. Every other ADR-0153 decision —
  scan-inferred schema, one virtual node per label sorted alphabetically,
  triple expansion over multi-label endpoints, negative virtual ids and the
  virtual-node side table, zero-rows empty behavior, property-less virtual
  relationships — stays in force.
- The coupling concern ADR-0153 raised (three renderings of the same
  catalog facts) is now **accepted**, mitigated by deriving all three from
  the same overlay-aware catalog accessors (`named_indexes()` /
  `named_constraints()`) so `SHOW INDEXES`, `SHOW CONSTRAINTS`, and
  `db.schema.visualization()` cannot observe different catalog states;
  only the string renderings differ, and the type-name mapping is pinned
  here.
- The rendered `Constraint(...)` strings carry no `id`/`ownedIndex`; any
  future schema-rule-id facility that adds them needs a new ADR.
- `docs/reference/cypher-syntax.md`'s `db.schema.visualization()`
  paragraph (which documents the empty placeholders) must change in the
  same feature.
- The graph-examples differential suite (ADR-0144) gains coverage of the
  populated rendering; the empty-placeholder expectation is removed.

## Later references

- **ADR-0187** supersedes Decision item 5 ("Virtual relationships keep
  carrying no properties"): `db.schema.visualization()` virtual
  relationships now carry a single `name` property equal to the
  relationship type (LABS-185). Every other decision here remains in
  force.
