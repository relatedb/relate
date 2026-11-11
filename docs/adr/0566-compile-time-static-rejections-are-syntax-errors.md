# ADR-0566: Compile-time static rejections carry the SyntaxError class

Status: Proposed
Claim: f072c3d86b9e55f8e11919c17a946e945675b159

## Context

Neo4j splits type-fault rejections by whether the type is knowable from
the statement text. A statically invalid argument — a literal of the
wrong type, a literal outside its value domain, a literal that cannot
parse — is rejected during semantic analysis and surfaces as
`Neo.ClientError.Statement.SyntaxError`. A value whose type is only known
per row (a property read, a `CASE` result) fails while rows are produced
and surfaces as `Neo.ClientError.Statement.TypeError`. Probed on Neo4j
2026.10.1 (2026-11-10): `char_length([1])` and
`char_length(n.p)` over a list property are the two classes,
`vector([1], 0, INTEGER)` and `date('2022-1')` are SyntaxError, and a
non-boolean predicate is SyntaxError statically (`WHERE 1`) but TypeError
dynamically.

The engine reported the later-stage class for the static half: some
front-end checks emitted a type-class detail token the classifier maps to
TypeError, and several statement shapes never reached a front-end check
at all, so a planner or executor site answered with TypeError or
SemanticError instead. The protocol-visible class is what drivers and the
compatibility harness read, so the split is recorded here rather than
left to each construction site.

ADR-0511 read the static half's class from the GQLSTATUS *cause* code
(e.g. 22NB1) instead of the driver's exception class, and concluded
TypeError. The class comparison the harness enforces is the driver class;
this ADR supersedes ADR-0511's static-site conclusion and leaves its
runtime half intact.

## Decision

1. A rejection decided from statically known types or values is a
   compile-time SyntaxError. This covers literal argument types (`vector`,
   `vector.similarity.*`, `char_length`, temporal patterns), literal
   value domains (vector dimension, `replace()` limit, path-selector
   count, `IN TRANSACTIONS` concurrency), literal parse failures, call
   arity, statically non-boolean predicates and operands, and the
   statement-shape rejections with no runtime input (empty input,
   multiple graph targets, page bounds outside RETURN/WITH).
2. A rejection that depends on a per-row value stays the runtime
   TypeError (or the argument-value class for value-domain faults).
3. The decision lives in the front-end static-literal validator, which
   owns "what the statement text decides"; the planner and executor keep
   only runtime backstops, so the two layers cannot disagree on a shape
   the front end rejects.
4. A feature gate's own rejection (preview not enabled) keeps its
   authored class; this ADR says nothing about gated constructs.

## Alternatives considered

- **Map the type-class detail tokens to SyntaxError globally.** Rejected:
  the same tokens carry genuine runtime faults
  (`char_length(n.p)`), which live Neo4j reports as TypeError.
- **Reclassify the executor's rejections only.** Rejected: the executor
  cannot see a literal's type through the plan-cache placeholder, and a
  runtime-decided value and a literal reach the same site.
- **Keep ADR-0511 and leave the corpus mismatch.** Rejected: the class is
  part of the Bolt/HTTP/DB-API surface; the ADR-0511 probe read cause
  codes, not the exception class.

## Consequences

- Statements that Neo4j rejects at compile time now fail before planning,
  so they fail on an empty graph too, and `EXPLAIN` reports them.
- The rendered class prefix moves from `type error:` / `semantic error:`
  to `syntax error:` for these rejections; message text inside is
  unchanged. Any consumer that matched the prefix must move.
- An error-class detail token is no longer sufficient evidence of a
  class: a token may be raised under either class depending on the stage,
  so tests assert the class, not the token.
- The preview gate keeps the graph-type CIP suites on their authored
  SemanticError until those features are enabled and their validation
  rules land.
