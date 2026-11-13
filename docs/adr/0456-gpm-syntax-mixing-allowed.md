# ADR-0456: GPM syntax mixing — the allowed combinations

Status: Proposed

Claim: 7e73d8091a6bc10dcab0fd9c28c5f4873df1e226

## Context

The Neo4j acceptance corpus pins three syntax-mixing behaviors CypherLite's
parser did not model (the `gpm-syntax-mixing` cluster of the syntax-gap
diagnosis, 51 scenarios):

1. **Label-syntax mixing under the extra-semantic-features switch.** With
   Neo4j's corresponding extra-semantic-feature switch
   (its GpmSyntaxMixingAllowedAcceptance suite), legacy
   colon-conjunction (`:A:B`), GQL label-expression symbols (`|`, `&`, `!`,
   `%`) and the `IS` introducer may mix across the label expressions of one
   clause (`(n:A:B)-[]-(m) WHERE m:(A&B)|C`, `(n IS S:R)`,
   `RETURN n:A&B, n:A:B`). Without the switch Neo4j — and CypherLite's
   frozen GPM syntax-mixing deviation rejections — error these combinations.
   Within a *single* label expression the mixing stays illegal even under
   the switch (`(n:A|B:C)`, `CREATE (n:A:B&C)`: 42I10).
2. **Ungated acceptances.** A `CALL {}` subquery body is one label-mixing
   scope of its own (mixing colon-conjunction in one of its statements with
   `IS`/GQL symbols in another is illegal, while the body never combines
   with the enclosing clause); a pattern
   comprehension's inner pattern is its own pattern context for the
   quantifier-mixing check (a legacy `[:R*1..2]` inside a comprehension
   nested in a quantified path pattern's WHERE is legal); and the legacy
   bracket-syntax variable-length form accepts both an inline `WHERE`
   predicate (`-[r*1..5 WHERE ...]->`) and a type expression containing
   negation or the wildcard (`-[r:!A&!B*1..5]->`) — each is the same AST the
   GPM postfix form already normalizes to and executes.
3. **Frozen-set fallout.** Accepting (2)'s type-expression forms makes four
   frozen cip-eval identities stop passing — the
   relationship-type-expression and quantifier-mixing deviation scenarios —
   because the queries now succeed where the scenarios
   expect a `SyntaxError` (cip-eval's lenient error matching counts only "an
   error occurred"). ADR-0119 set the precedent: retiring a frozen identity
   for a dialect decision is an ADR-level change, not a silent baseline edit.
   The quantifier-mixing identity follows from ADR-0471: one traversal
   engine for every
   var-length spelling evaluates the legacy bracket inline-`WHERE` form
   exactly like its GPM twin, so it now yields `count(*)` 0 instead of the
   incidental evaluation error that used to satisfy the scenario leniently.

## Decision

- **`mixed-label-syntax` preview row (ADR-0371 registry).** Neo4j's
  mixed-label-syntax allowance is modeled as the
  `CYPHERLITE_PREVIEW=mixed-label-syntax` opt-in. The gate widens what the
  parser accepts, so it threads into the parse itself as
  `cypherlite_parser::ParseOptions` (`parse_with_options`; the front-end's
  preview admission builds the options from the store-resolved set) — the
  obfuscated-literals precedent: a gate that lifts a floor rejection keeps
  the floor's error when off, rather than the authored preview-disabled
  text. Under the opt-in, `:` parses as an `&`-level conjunction operator in
  `parse_label_and` (so `IS S:R` = `S & R`) and the legacy colon-chain
  fast-path is bypassed, while a *within-one-expression* mixture stays
  rejected: `parse_label_expr_body` resets and checks the two
  expression-scoped flags (`label_expr_saw_legacy_conjunction`,
  `label_expr_saw_gql_symbol`) around each label expression, so
  `(n:A|B:C)` and `CREATE (n:A:B&C)` keep their 42I10 while
  `(n:A:B)-[]-(m) WHERE m:(A&B)|C` is accepted. Off (the floor), every
  syntax-mixing rejection keeps firing unchanged.
- **A `CALL {}` body is one label-mixing scope, ungated.**
  `parse_call_subquery_statement` saves the clause flags, clears them for
  the body, and checks them once at the body's end, so two clauses of one
  CALL body combine (Neo4j GpmSyntaxMixingAcceptance "Conflicting syntax in
  separate statements within a CALL subquery") while the body never
  combines with the enclosing clause. `EXISTS {}`/`COUNT {}`/`COLLECT {}`
  subquery *expressions* keep bubbling their label usage into the enclosing
  clause (label-expression mixing into the enclosing clause stays an error).
- **Pattern comprehensions isolate quantifier context, ungated.** The
  clause-level quantifier-mixing flags split: a comprehension's inner
  pattern accumulates into comprehension-scoped flags that never combine
  with the clause's own forms but do combine across comprehensions of the
  same clause (quantifier mixing across comprehensions of one clause stays
an error).
- **Bracket var-length accepts inline WHERE and full type expressions,
  ungated.** The `RelationshipPatternPredicateInVariableLength` and
  `VariableLengthRelationshipWithTypeExpression` parser rejections are
  removed: both spellings normalize to the AST the GPM postfix form already
  produces and the executor already runs. ADR-0471's one traversal engine
  makes the inline-WHERE form evaluate too (per-element post-filter, so
  `MATCH ()-[r*1..5 WHERE r.p > 30]->()` yields 0 rows like its GPM twin),
  which both satisfies the CIP text and retires the last lenient pass.
- **Frozen set and baseline.** The four identities above are removed from
  `testdata/cip-eval/ENGINE_PASSING.json`; `baselines/cip-eval.json` lowers
  the relationship-type-expression suite to 87/89 and the
  quantifier-mixing suite to 109/111.

## Alternatives considered

- **Model the switch as a new runtime configuration knob instead of a
  preview row.** Rejected: ADR-0371 already owns "syntax the floor rejects
  becomes acceptable behind an opt-in", the harness already maps the Neo4j
  `@conf` tag onto a `CYPHERLITE_PREVIEW` name, and a second opt-in channel
  would be a parallel table for one fact.
- **Gate the mixed forms with the authored preview-disabled error text at
  the front-end (AST detectors, the `gql-for` pattern).** Rejected: the
  floor behavior is a *rejection*, so there is no floor AST to detect the
  gated construct in; the parser must know the opt-in before it can produce
  an AST at all. The `ParseOptions` threading is the same shape the
  obfuscated-literals gate uses.
- **Keep the bracket var-length rejections and retire only the label-mixing
  half of the node.** Rejected: the acceptance corpus pins the bracket forms
  as the same ASTs their GPM twins normalize to; keeping a spelling-only
  rejection contradicts the postfix form's own normalization rule.
- **Relax the label mixing ungated.** Rejected: the frozen
  error-expecting identities (locations.feature [22],
  node-label-expressions [24]/[25], the whole label-expression-mixing
  deviation family) would stop passing without any opt-in, a much larger
  unrequested dialect change.

## Consequences

- The dialect floor is unchanged except for the three ungated acceptances;
  the TCK pin and every other frozen cip-eval identity keep passing.
- Neo4j's GpmSyntaxMixingAllowedAcceptance suite becomes runnable by
  setting `CYPHERLITE_PREVIEW=mixed-label-syntax` (the harness's conf-tag
  mapping does exactly this).
- A subquery expression nested inside a CALL body bubbles into the CALL
  body's one scope, not into the enclosing clause; no pinned scenario
  exercises that combination, and the simplification is accepted knowingly.
- The `[14]` DEVIATION row (inline WHERE on bracket var-length) stays
  frozen and passing leniently: the form parses but fails at evaluation
  (`r` is a list). When per-hop inline predicates are implemented, that row
  must be retired like the three here.
