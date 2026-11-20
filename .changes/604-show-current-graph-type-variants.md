### Added

- `SHOW CURRENT GRAPH TYPE AS CONSOLIDATED` and `AS FLATTENED` (CIP-275,
  ADR-0638), behind the new `RELATE_PREVIEW=graph-type-show-variants`
  opt-in. `AS CONSOLIDATED` shows independent property-existence and
  property-type constraints as the element types they would be; `AS
  FLATTENED` shows each node element type with its inherited labels,
  properties, defaults and undesignated constraints inlined. Both are
  display-only, work with `AS GRAPH`, `YIELD` and `WHERE`, and leave the
  stored graph type untouched; without the opt-in they fail with the
  authored preview error.
