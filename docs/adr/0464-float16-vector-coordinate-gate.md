# ADR-0464: FLOAT16/BFLOAT16 vector coordinate types gate behind a preview row

Status: Proposed

Claim: 3fd54950ae0d2c6b3fb7215bf0ffa4e0630e184d

## Context

The Neo4j acceptance corpus models the two narrow vector coordinate types
(`FLOAT16`, `BFLOAT16`) as a *deployable switch*, Neo4j's own
configuration flag. `VectorValueAcceptance`
pins both sides — six scenarios under the suite's disabled `@conf` rows expect a
rejection when a `VECTOR<…>` type expression or a `vector(<list>, <dim>,
<type>)` constructor names one of the two kinds, and the same suite's
positive rows (type predicates over a FLOAT16 vector, the constructor's
round-tripped `{"type": "FLOAT16"}` value) carry no `@conf` tag at all.

CypherLite's `function-registry` node of the syntax-gap round added both
kinds ungated, so the two sides of that switch cannot be represented: one
engine configuration accepts them everywhere, and the disabled rows fail
with "expected an error but the query succeeded". ADR-0371 already owns
exactly this shape — a language construct the floor does not pin, switched
on per store through `CYPHERLITE_PREVIEW`.

## Decision

- **New preview row `float16-coordinate-types`** in the ADR-0371 registry
  (`cypherlite-core/src/preview.rs`), named after the Neo4j configuration's
  own subject. The row is the second *acceptance-widening* gate whose
  disabled form is the authored error rather than a floor rejection: the
  parser already accepts the two kinds, so nothing floor-level distinguishes
  them from `FLOAT32`.
- **Gate point: statement admission, detected on the parsed AST**
  (`cypherlite-front`'s preview gate loop). A statement uses the gated
  feature when either
  - a `<value type>` names `FLOAT16`/`BFLOAT16` as a vector coordinate
    kind, at any nesting depth — a type predicate (`x IS :: VECTOR<FLOAT16>(3)`),
    a property-type constraint, or a graph-type element property
    type; or
  - the `vector()` constructor's third argument is one of the two kinds
    (the parser stores that keyword as a coordinate-name string, resolved
    through the model's one coordinate-type table rather than re-spelled).
  Both arms test the `VectorCoordKind` vocabulary itself, so a later
  coordinate kind cannot be gated or ungated by accident.
- **Both directions are authored errors.** With the row enabled the
  construct parses, lowers and evaluates exactly as today; disabled it
  fails closed with ADR-0371's
  `preview feature 'float16-coordinate-types' is not enabled: set
  CYPHERLITE_PREVIEW=float16-coordinate-types`. `FLOAT32`, `FLOAT`,
  `INTEGER32` and the other coordinate kinds stay on the dialect floor —
  the gate covers only the `FLOAT16`/`BFLOAT16` pair.

## Alternatives considered

- **Gate at parse, as `mixed-label-syntax` does (a third `ParseOptions`
  flag).** Rejected: the floor *accepts* these keywords, so the disabled
  behavior would have to be a parser-syntax error the floor never produced,
  and the authored ADR-0371 text would have to be re-created in the parser
  crate, which does not know the store's resolved preview set. One policy
  through the front's existing gate loop is the smaller surface.
- **Leave the kinds ungated and file the six rows as harness skips.**
  Rejected: the corpus pins both sides of a real Neo4j configuration, and a
  skip leaves the disabled half of that contract unrepresented in the
  engine, contradicting the round's rule that a corpus-visible dialect
  switch is modeled, not skipped.
- **Reject FLOAT16/BFLOAT16 always (drop the positive rows from the
  pin).** Rejected: the positive rows are already-passing identities of the
  same suite, and the pair is accepted into the language; losing them would
  trade one unrepresented configuration for the other.

## Consequences

- The dialect floor is unchanged for every other coordinate kind; the TCK
  pin and the frozen cip-eval identities keep passing (none uses FLOAT16).
- The repo's own pins split across two test binaries —
  `float16_coordinate_types_gate.rs` (opt-in: the kinds stay distinct,
  constraints lower) and `float16_coordinate_types_closed.rs` (no opt-in:
  the authored error for the constructor, nested type expressions and
  constraint types) — because `CYPHERLITE_PREVIEW` is read once per
  `Database::open` and is process-global.
- The acceptance harness needs two things this ADR does not ship:
  mapping the corpus's float16 configuration key onto
  `float16-coordinate-types` in `environment.py` (the positive rows carry
  no `@conf` tag, so they run gate-closed by default), and accepting the
  authored `SemanticError` class for the disabled rows, whose `42001` row
  asks for `SyntaxError`. ADR-0371 makes the authored error a semantic
  error on purpose; the harness-side class expectation is the harness's to
  adjust, not the engine's to bend.
