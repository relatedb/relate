# Graph type SHOW variants example

Run with `cargo run -p relate --example graph_type_show_variants`.

`SHOW CURRENT GRAPH TYPE` has two display variants (CIP-275, ADR-0371
preview gate `graph-type-show-variants`; ADR-0638). Both are views of the
stored graph type: nothing is altered, created or persisted, and the plain
`SHOW CURRENT GRAPH TYPE` is byte-identical before and after.

- `AS CONSOLIDATED` shows every independent property-existence and
  property-type constraint (one on a label or relationship type that
  identifies no element type) as a property of an element type for its
  label, creating one where the label has none. Undesignated key and unique
  constraints stay constraints.
- `AS FLATTENED` shows each node element type with everything it inherits
  through its implied labels, transitively: the full implied-label set, the
  effective properties (inherited defaults included) and the undesignated
  constraints of every ancestor. Independent constraints on non-identifying
  implied labels inherit as if they were dependent. Edge element types and
  relationship endpoints are untouched.

The example opts in with `RELATE_PREVIEW=graph-type-implied-labels,graph-type-show-variants`
before `Database::open` (the first for the inheritance the example
declares), sets a graph type with an inheritance edge and two independent
constraints, shows both variants as specification strings, views one as a
graph (`AS GRAPH`, which takes `SINGLE | SEPARATE` as before), and checks
the plain specification is unchanged afterwards.

The variants are mutually exclusive and are written before `AS GRAPH`;
both words together, or a repeated variant, is a parse error. Without the
opt-in either variant fails with the authored error:

```text
semantic error: SemanticError: preview feature 'graph-type-show-variants' is not enabled: set RELATE_PREVIEW=graph-type-show-variants
```
