---
type: adr
area: engine
claim: e6c9a08179e2ca7339e74804872cfcba6e30eea8
name: 0338-arithmetic-failure-display-payload
status: proposed
tags:
  - errors
title: ArithmeticFailure renders its bare payload; Error::Arithmetic composes the legacy text
REFERENCES:
  - "[[adr/0307-bolt-failure-carries-neo4j-status-codes]]"
---

# ADR-0338: ArithmeticFailure renders its bare payload; Error::Arithmetic composes the legacy text

## Context

[ADR-0307](./0307-bolt-failure-carries-neo4j-status-codes.md) typed the arithmetic failures as `ArithmeticFailure` and made
`Error::Arithmetic(ArithmeticFailure)`'s Display the historical full text.
The current split puts the class prefix in the inner type:
`ArithmeticFailure`'s own `Display` writes `type error: / by zero` for the
two division failures, while `Error::Arithmetic`'s attribute is a bare
`{0}`. The inner public type's rendering contract therefore leaks the
enclosing variant's class prefix, and the same `type error:` text is
spelled twice — once by `Error::Type`'s `#[error]` attribute and once by
`ArithmeticFailure`'s `Display` — so [ADR-0307](./0307-bolt-failure-carries-neo4j-status-codes.md)'s byte-identity pin rests on
two independent spellings.

## Decision

The class prefix lives at the `Error` variant, composed from one shared
constant; `ArithmeticFailure`'s `Display` renders only its payload
(`self.detail()`).

1. `Error::Arithmetic`'s attribute becomes
   `#[error("{}", arithmetic_detail(.0))]`; `arithmetic_detail` composes
   the legacy full text: `TYPE_ERROR_PREFIX: / by zero` and
   `TYPE_ERROR_PREFIX: % by zero` for the division failures, the bare
   `ArithmeticError: rounding necessary` for `RoundingNecessary`.
2. `TYPE_ERROR_PREFIX` (the `type error` token) is the one spelling,
   consulted by `Error::Type`'s `#[error("{}", type_error_display(.0))]`
   and by `arithmetic_detail` — the `CONSTRAINT_VALIDATION_FAILED`
   pattern.
3. `ArithmeticFailure`'s `Display` becomes `f.write_str(self.detail())`:
   `/ by zero`, `% by zero`, `ArithmeticError: rounding necessary`.
   `Error::Arithmetic(…).to_string()` is byte-identical to before, so the
   `cypherlite::Error` rendering, the Bolt status mapping
 (`ArithmeticFailure::detail` is what [ADR-0307](./0307-bolt-failure-carries-neo4j-status-codes.md)'s mapper reads) and the
   TCK bridge do not move. `ArithmeticFailure::detail()` is unchanged.

## Alternatives considered

- **Keep the prefix in `ArithmeticFailure`'s Display and single-source the
  constant only there.** Rejected: the inner public type keeps rendering
  its enclosing variant's class, so a consumer of `ArithmeticFailure`
  alone sees a prefix the type does not own.
- **Move the full text into `ArithmeticFailure`'s Display and make
  `Error::Arithmetic` a bare `{0}` (the status quo).** Rejected: that is
  the leak this ADR removes, and the prefix stays spelled twice.
- **Drop the prefix from both renderings (always bare).** Rejected: it
 changes `Error::Arithmetic(…).to_string`, breaking [ADR-0307](./0307-bolt-failure-carries-neo4j-status-codes.md)'s
  byte-identity contract and the pinned legacy texts.

## Consequences

- `ArithmeticFailure::DivisionByZero.to_string()` changes from
  `type error: / by zero` to `/ by zero` (documented public API);
  `Error::Arithmetic(…).to_string()` and `Error::Type(…).to_string()` are
  unchanged.
- Tests: `error.rs` pins both renderings plus the shared prefix; the
  docs `docs/api/rust/errors.md` row for `Arithmetic` is updated.
