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

`round(value, precision [, mode])` rounds the shortest decimal spelling
(`round_decimal` in `relate-executor/src/eval/numeric.rs`), so a precision at
or above the value's own decimal scale is the identity by construction — the
overflow-prone `powi` scale that once produced `round(2.5, 400) = NaN` is
gone. The first revision of this ADR capped the precision at 15 on the f64
significant-digit rationale; that cap is a divergence from the reference
engine, measured live on Neo4j 2026.10.1:

- `round(1.9951, 2147483647)` is `1.9951` — any int32 precision is accepted,
  and one at or above the decimal scale returns the value unchanged (no zero
  padding, no error).
- A negative precision is the ArgumentError class (GQL 22N38):
  `Precision argument to 'round()' cannot be negative`.
- A precision above the int32 range is the ArgumentError class (GQL 22003):
  `Invalid input for precision value in function 'round()': Expected an
  integer between -2147483648 and 2147483647, but got: <n>`.

## Decision

`round()`'s precision is bounded to the int32 range `0..=2147483647`,
matching the reference engine: a precision in range is accepted as-is
(`round_decimal` returns the identity once the scale is reached), a negative
one fails with the ArgumentError `cannot be negative` text, and one above
`i32::MAX` fails with the ArgumentError naming the int32 bounds. The default
precision stays `0`.

## Alternatives considered

- **Cap at 15 (this ADR's first revision).** Rejected: the NaN rationale died
  with the `powi` scale — `round_decimal` cannot overflow — and the cap
  rejects queries the reference engine answers (`round(x, 500)`, the corpus
  row `round(3.141592653589793, 500)`).
- **Bound at 308 (`10^p` finite in f64).** Rejected with the 15-cap: the
  same overflow premise, already gone.
- **Keep the current behavior and document NaN.** Rejected: `NaN` is a
  fabricated value over an input error, and no surface expects it.

## Consequences

- `round(x, p)` for `16 <= p <= 2147483647` returns the value (identity once
  the decimal scale is reached, exact rounding below it) instead of erroring;
  `round(x, p)` for `p > 2147483647` errors with the bounds text; negative
  precision keeps the ArgumentError class with the reference engine's
  message.
- Tests: `numeric.rs` unit tests and the `round_precision` integration file
  pin both edges, the identity at high precision, and the unchanged
  in-range values; the corpus rows for high precision now pass.

## Later references

- First revision (same number, never accepted): bounded the precision to
  `0..=15` on the f64 significant-digit rationale while the implementation
  still scaled by `powi`. Superseded by the live-verified int32 contract
  above once `round_decimal` made the overflow premise stale.
