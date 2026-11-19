---
type: adr
area: cypher
claim: 99390f3dada2429e3377caafe2b0591085e5ba1c
name: 0336-round-precision-bound
status: proposed
tags:
  - functions
title: round bounds its precision argument
---

# ADR-0336: round() bounds its precision argument

## Context

`round(value, precision [, mode])` scales by `10_f64.powi(precision)` and
divides back (`cypherlite-executor/src/eval/numeric.rs`). The precision
boundary is half-checked: a negative precision fails loudly with
`NumericValueOutOfRange: round precision must be non-negative`, but any
large positive integer is accepted and silently degrades — `*value as i32`
saturates, `powi` overflows to `inf`, and `round(2.5, 400)` returns `NaN`
instead of a value or an error. The engine's `f64` backing carries at most
15–17 significant decimal digits, so a precision beyond that cannot express
anything the engine does not already have.

## Decision

`round()`'s precision is bounded to `0..=15`, symmetric with the negative
arm: a precision outside that range fails with
`Error::execution("NumericValueOutOfRange: round precision must be at most 15")`
(a positive precision above 15) or the existing
`NumericValueOutOfRange: round precision must be non-negative` (below 0).
The bound is a named constant next to `round_function` with the f64
significant-digit rationale, so the scale computation cannot overflow and
the saturating `as i32` is gone. The default precision stays `0`.

## Alternatives considered

- **Bound at 308 (`10^p` finite in f64).** Rejected: `value * scale` still
  overflows to `inf` for large values at high precisions, so the bound
  would not deliver the "never a fabricated result" contract.
- **Return `value` unchanged above the safe range (Neo4j's BigDecimal
  behavior).** Rejected: it silently ignores the requested precision, and
  the divergence is invisible in the result; the engine prefers an honest
  range error to an input-dependent identity.
- **Compute with an overflow-free algorithm and no bound.** Rejected as
  the smaller change: the scale-multiply is the one implementation, and a
  bound is the explicit statement of what the engine supports.
- **Keep the current behavior and document NaN.** Rejected: `NaN` is a
  fabricated value over an input error, and no surface expects it.

## Consequences

- `round(x, 16..)` now errors; `round(x, 0..=15)` is unchanged, including
  the mode handling and the `UNNECESSARY` failure.
- Tests: `numeric.rs` unit tests pin the bound (both edges) and the
  existing values; the TCK and CIP-eval use precisions in range, so their
  gates do not move.
