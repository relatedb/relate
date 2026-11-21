---
type: adr
area: engine
claim: 1716a86ae3e8d090eede24fe643ae666dad25259
date: 2026-11-21
deciders: MemoryDB maintainers
name: 0639-procedure-call-failed-class
status: proposed
tags:
  - conformance
  - errors
title: A registered call failing its own signature carries the ProcedureCallFailed class
REFERENCES:
  - "[[adr/0565-argument-and-procedure-error-classes]]"
  - "[[adr/0502-typed-c-abi-v2]]"
  - "[[adr/0482-spec-harness-coverage-lane]]"
---

# ADR-0639: A registered call failing its own signature carries the ProcedureCallFailed class

## Context

The spec harness enforces the openCypher class the language bindings expose
(`tck_class`, [ADR-0482](./0482-spec-harness-coverage-lane.md)). Two corpus
rows failed on it, and the live oracle disagrees with the engine for a whole
family of faults — every runtime `Invalid call signature` rejection the
temporal functions raise (each shape probed 2026-11-20 on Neo4j 2026.10.1):

- `duration(123, "s 'seconds'")`, `duration_between('abc', 'def')`,
  `duration.inMonths('abc', 'def')`, `date(123)`, `localdatetime(123)`,
  `duration(123)`, `date(duration('P1D'))`, `datetime.truncate('millennium',
  'abc')`, `date.truncate('millennium', 123)` and `date.realtime(123)` —
  live answers `Neo.ClientError.Procedure.ProcedureCallFailed` under
  GQLSTATUS **42001** for every one.
- The engine answered three different ways: the `(value, pattern)` value
  arm a `TypeError`, `between_call` and the rest a `SyntaxError`.

Live splits the `(value, pattern)` operand faults by side: a non-STRING
*pattern* (`date("20150721", 42)`) stays `Neo.ClientError.Statement.
SyntaxError`'s type mismatch — that arm keeps its class.

## Decision

One typed variant owns the family: `Error::ProcedureCall(String)` — the
registered call's own runtime failure.

- Binding metadata reports kind `ProcedureCallFailed`, class
  `ProcedureError` (an existing openCypher TCK class).
- Bolt code `Neo.ClientError.Procedure.ProcedureCallFailed`; GQLSTATUS
  `42001`, `Runtime` stage, not retryable.
- Every runtime temporal `Invalid call signature` rejection constructs it;
  sites whose corpus rows pass today and whose live behaviour was not
  probed are not swept in by assumption.
- [ADR-0565](./0565-argument-and-procedure-error-classes.md)'s freeze of
  the unregistered-procedure GQLSTATUS at `42N06` stands: that site has a
  legacy wire obligation; this variant has none, so it records live's
  `42001`.
- The `ProcedureError:` Display token agrees with the class the variant
  carries, so a reader of the rendered message and a client reading the
  wire code are told the same class.

## Consequences

- The Python/C/JS/Go/Swift bindings' `kind`/`tck_class` move for these
  sites; the Bolt code and GQLSTATUS move with them, all derived from the
  one variant.
- Several harness corpus rows asserted the engine's old classes rather
  than live's — the constructor rows for a non-STRING value or pattern,
  the `duration_between` non-temporal row, and the out-of-range
  `replace()` limit rows — and are corrected in the corpus in the same
  wave.
- The stringly `Invalid call signature` spellings that remain
  (front-end static rejections) keep the syntax class; the phrase no
  longer implies a class — the raise site's variant does.

## Alternatives considered

- **Keep the per-site classes.** Rejected: the corpus, live, and the
  family's own internal consistency all disagree with the split.
- **Route through the stringly `ProcedureError:` nested-class token.**
  Rejected: the token scan is the mechanism for still-stringly payloads
  ([ADR-0502](./0502-typed-c-abi-v2.md) §2); a new typed
  site gains nothing from re-entering it and loses the typed kind.
- **GQLSTATUS `42N06` for family consistency with
  `ProcedureNotFound`.** Rejected: that mapping is a recorded freeze of
  legacy wire output, not a preference; a new variant with no legacy
  obligation records what live sends.
