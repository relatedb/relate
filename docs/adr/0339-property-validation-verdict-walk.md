---
type: adr
area: engine
claim: e79fca59d4f8ed4736cd67ef42ef0de033b054ff
name: 0339-property-validation-verdict-walk
status: proposed
tags:
  - errors
  - write-path
title: Property-write validation is one typed verdict walk in a shared exec module
REFERENCES:
  - "[[adr/0327-engine-statement-scoped-bytes-value]]"
---

# ADR-0339: Property-write validation is one typed verdict walk in a shared exec module

## Context

The property-write validation policy ([ADR-0327](./0327-engine-statement-scoped-bytes-value.md)) lived in
`cypherlite-executor/src/exec/create.rs` and was consumed sideways by
`exec/write.rs` (`use super::create::{check_property_value, sanitize_props}`)
— a helper used by siblings living in the first caller's module. The
policy was also encoded twice: `valid_property_value` walked the value and
returned a bare `bool`, and `check_property_value` re-walked the value with
a shallow scan to pick the error detail. The shallow scan classified a
direct `Bytes` item in a list but not a byte array nested deeper
(`[[bytes]]` reported the generic `InvalidPropertyType` instead of the
byte-array `TypeError`), and `valid_property_value` re-listed all six
`VectorCoordKind` variants — the enum's entire vocabulary — so a seventh
coordinate kind would compile and be silently rejected.

## Decision

One typed verdict walk owns the policy, in a shared
`cypherlite-executor/src/exec/props.rs` module consumed by `create.rs` and
`write.rs` as siblings.

1. `property_value_invalidity(v) -> Option<PropertyInvalidity>` is the one
   walk: `None` when `v` can be stored, `Some(HoldsBytes)` when a byte
   array appears anywhere inside the value (a list nested at any depth
   included), `Some(Other)` for every other non-storable shape. A list is
   storable when its non-null items are one level of homogeneous scalar
   values; a nested list of scalars stays `Other`.
2. `check_property_value(v)` matches the verdict:
   `HoldsBytes` → the typed `TypeError` with `BYTES_PROPERTY_DETAIL`
   (module-private to `props.rs`), `Other` → `Type("InvalidPropertyType")`.
   `valid_property_value` is deleted; `sanitize_props` moves to
   `props.rs`.
3. `Value::Vector { .. }` stores without re-listing `VectorCoordKind`:
   the policy is "any vector value stores".
4. The null-transparency contract lives once, on
   `check_property_value` (`Value::Null` is `Ok`): `write.rs` drops its
   dead `!val.is_null()` guards instead of re-stating the policy per call
   site.

## Alternatives considered

- **Keep the bool and add a deeper bytes scan.** Rejected: validity and
  why-it-failed stay two facts encoded in two walks that can drift.
- **Delete `check_property_value` and return `Result<(), Invalidity>` from
  the walk.** Rejected: callers want the rendered `Error`, and the
  verdict walk stays testable without constructing errors.
- **Match the six `VectorCoordKind` variants explicitly.** Rejected: the
  listing encodes no subset policy, and a new coordinate kind would be
  silently rejected here while `typematch` accepts any vector.
- **Keep the helpers in `create.rs`.** Rejected: `write.rs` already
  imports them sideways, against the AGENTS layering rule.

## Consequences

- `[[bytes]]` as a property value now reports the byte-array `TypeError`
  instead of `InvalidPropertyType`; both were already errors and nothing
  is written either way.
- Tests: `exec/props.rs` unit tests cover scalars, vectors, homogeneous
  and heterogeneous lists, nulls, `[bytes]` and `[[bytes]]`;
  `cypherlite/tests/bytes_value.rs` pins the nested case through the
  public API.
