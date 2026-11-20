---
type: adr
area: cypher
claim: 4d6a0e505bc70d4267aab40a7c844fa4960e6fa5
date: 2026-11-20
deciders: the maintainers
name: 0631-dynamic-slice-bounds-saturate-into-int32
status: proposed
tags:
  - conformance
title: Dynamic list- and string-slice bounds saturate into the int32 range
REFERENCES:
  - "[[adr/0175-list-and-string-form-conversions]]"
  - "[[adr/0559-runtime-argument-value-error-variant]]"
---

# 0631: Dynamic list- and string-slice bounds saturate into the int32 range

## Context

[ADR-0175](./0175-list-and-string-form-conversions.md) decided the
list-slice bound policy against live Neo4j 2026.09: "Bounds truncate
toward zero, NaN → 0, and saturate; each bound must then fit int32 (else
`NumberOutOfRange`)" — the `asIntExact` image its probes read there. A
2026-11 parity probe of Neo4j dev (the same site, dynamically-evaluated
FLOAT bounds) got answers, not errors:

- `RETURN [1,2,3][head([1.0/0.0, 'x'])..2]` → `[]`
- `RETURN [1,2,3][head([2147483648.0, 'x'])..]` → `[]`
- `RETURN [1,2,3][head([9.3e18, 'x'])..]` → `[]`

Dev's dynamic bound conversion saturates into the int32 range before the
slice clamps to the list, so an oversized bound reads as "past the end",
never as a range fault. The same report re-confirmed the neighbors stay
rejections: statically FLOAT-typed bounds fail before execution, and
`substring()` / `left()` / `right()` and list indexing still reject the
equivalent values.

## Decision

Dynamic (runtime-evaluated) slice bounds — the one `slice_bounds` helper
both list slicing and string slicing read — saturate into the int32
range: the FLOAT bound converts with the shared Java `(long)` cast
([ADR-0175](./0175-list-and-string-form-conversions.md)'s
`float_to_i64`, unchanged), clamps to `[i32::MIN, i32::MAX]`, and then
the existing negative-index and `[0, len]` clamping applies. A dynamic
bound no longer raises `NumberOutOfRange`. Everything else keeps
[ADR-0175](./0175-list-and-string-form-conversions.md)'s rule:

- statically-known FLOAT bounds (float literals at plan time) keep the
  plan-time `InvalidArgumentType` rejection;
- `substring()` / `left()` / `right()` and list indexing keep the
  int32-fit `NumberOutOfRange` rejection;
- INTEGER slice bounds are longs and clamp at the end, as today.

Neo4j's list-slice and string-slice bounds share the conversion; the
report probes lists only, and Relate's one helper serves both — recorded
here as the derived share, not a separately probed fact.

## Consequences

- `eval::expr::slice_bounds` drops the FLOAT int32-fit error; its doc
  comment and the `NumberOutOfRange: slice start` / `slice end` pins
  move to the saturation behavior.
- [ADR-0175](./0175-list-and-string-form-conversions.md)'s List slice
  decision bullet is superseded for the dynamic-bound case: "each bound
  must then fit int32" applies to no dynamic bound, only to the
  unchanged neighbor sites. A **Later references** note on
  [ADR-0175](./0175-list-and-string-form-conversions.md) records this.
- [ADR-0559](./0559-runtime-argument-value-error-variant.md)'s
  argument-value error family loses the list-slice bound members (the
  kind stays at the other integer-argument sites).
- `docs/reference/functions.md` (list index and slice bounds) and
  `docs/api/rust/errors.md` (the `ArgumentValue` row's site list) update
  with the change.
- Engine tests re-pin the three report probes plus the negative-direction
  and string-slice forms; relate-tck pins land in the compat harness's
  gap-report feature.
