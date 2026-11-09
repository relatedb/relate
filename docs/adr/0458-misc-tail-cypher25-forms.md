# ADR-0458: Misc-tail Cypher 25 forms: list predicates, DISJOINT BY, name-matched UNIONs, ORDER BY projection scope, and procedure argument options

Status: Proposed

Claim: af42c9bd863190b06795b80903c498a854c6d873

## Context

The `fix-parser-syntax-gaps` feature's last node clears the remaining
`class-syntax-error` rows of the Cypher conformance harness (Neo4j's
acceptance suites plus small TCK clauses). Those rows mix genuinely missing
syntax with semantically rejected Cypher 25 forms, and two of them consume
the *same* query the pinned openCypher 2024.3 TCK expects to fail. The
authority ordering is ADR-0002's: a CIP-verified / current-Neo4j behaviour
outranks the 2024.3 TCK floor. This ADR records that batch's dialect,
procedure and conformance-pin decisions.

## Decision

1. **List predicates in argument position.** `all(x IN list WHERE p)` parses
   as the list predicate even inside a function call's argument list; the
   enclosing call's explicit `f(ALL x)` set-quantifier spelling no longer
   swallows the leading `ALL`. One predicate
   (`Parser::all_is_set_quantifier`) states the rule for every
   set-quantifier site.

2. **CIP-260 `DISJOINT BY`.** `CALL (...) { ... } IN CONCURRENT TRANSACTIONS
   ... DISJOINT BY (<exprs> | AUTO | NONE)` parses and executes; it is legal
   only on the `CONCURRENT` form. The clauses are a batching hint that never
   changes results, and the engine already executes `IN TRANSACTIONS`
   sequentially, so accepting the hint is a no-op beyond parsing.

3. **List-comprehension property targets.** `SET`/`REMOVE` accept a property
   target whose base expression starts with `[` (`SET [x IN ns WHERE x.p = 1][0].p = -1`,
   TCK Set7/Remove4 [8]), the same general complex-target form the engine
   already accepted for function-call bases.

4. **Map projections are named after their variable.** An unaliased `RETURN
   n {.prop, .foo}` projects a column named `n`, so a `CALL (...) { ... }`
   body exports it under the variable's name (SubqueryAcceptance "Map
   projections … are OK").

5. **UNION branches match by column name.** Branches projecting the same
   column names in a different order are one result shape; the executor
   reorders each branch's rows onto the first branch's columns. Validation
   compares the names as a multiset; anything else stays the typed
   `DifferentColumnsInUnion`/`UNION requires same columns` error.

6. **ORDER BY projection scope.** Under `RETURN DISTINCT` (and an
   aggregating projection) an ORDER BY key resolves against the clause's
   projected *expressions*: a key may decompose any expression the clause
   projects (`RETURN DISTINCT {p: 1} AS pa0, pa0.p AS pa1, x AS y ORDER BY
   -1 = pa0.p + x`), while a variable leaf no projected expression contains
   stays `UndefinedVariable`. An ORDER BY expression that mixes aggregation
   with a non-aggregate part verbatim equal to a projection item is that
   grouping key (the acceptance ReturnOrderBy6 [5] "This works in LTS"
   shape).

7. **List/path to boolean.** A *runtime* list or path operand of
   `AND`/`OR`/`XOR`/`NOT` converts to a boolean (empty list false, non-empty
   list and any path true); the conversion is deprecated in Cypher 25. A
   list *literal* operand stays a compile-time `InvalidArgumentType` error,
   as openCypher TCK Boolean1-3 [8] pins. The same runtime conversion is the
   predicate policy of the boolean predicate positions — a `MATCH`’s and
   `WITH`’s `WHERE`, the `FILTER` clause, the aggregating `HAVING`, the
   GPM route filter and the pattern engines’ per-trial `WHERE` (a QPP
   leg), and the list/map comprehension `WHERE`s: a predicate value that
   is a runtime list or path converts, a null does not hold, and any other
   non-boolean is an `InvalidArgumentType` — a property read has no
   statically known type, so the check cannot fire before the value is
   known (Neo4j’s `listCoercedToBooleanCheck` exempts it and the runtime
   converts). The list-quantifier’s inner `WHERE` predicate keeps the
   boolean-or-null rule alone, so a list or path there is an
   `InvalidArgumentType` error rather than a truthiness. A recorded
   divergence: `SHOW … YIELD … WHERE` over a non-boolean column is
   statically rejected by the reference server (22N27/22N37 against the
   column’s known type); this engine has no static column typing, so the
   row simply does not match there.

8. **`db.schema.visualization` options.** The optional argument accepts a
   `BOOLEAN` (Cypher 25) or the legacy options `MAP` (contents ignored).
   `true`/omitted/`MAP` report the synthesized relationship combinations,
   `false` only the combinations that exist in the data, `null` nothing.
   `value_matches_proc_type` learned a `A|B` union type for this.

9. **`db.index.fulltext.queryRelationships`.** Implemented alongside
   `queryNodes`, matching relationship-index types and yielding
   `(relationship, score)`. The Cypher 25 deprecation notification is
   deliberately not attached: the engine derives notifications at plan time
   only, and deprecating the pair would contradict the @neo4j-verified
   CIP-157 `deprecatedBy` inventory (7 replacements). The acceptance
   scenarios that expect the notification stay notification-missing, not
   syntax errors.

10. **MERGE self-reference.** A MERGE property map may read the entity it
    merges (`MERGE (a {prop: a.prop})`). The match uses a pattern whose pure
    self-tautologies are dropped; the original pattern still drives the
    create path, where the self-reference fails closed. The shape emits the
    deprecated 01N00 `MergeSelfReference` statement notification.

11. **`WITH *, x` in a scoped CALL body.** Re-listing an imported name
    beside a `*` is legal and does not promote it to star-visible: the
    body's `RETURN *` still exports only its own bindings.

12. **TCK pin.** Four scenarios whose expectations Neo4j's current Cypher 25
    behaviour reverses are removed from the vendored 2024.3 pin (inline
    commented, ADR-0188's mechanism): ReturnOrderBy6 [5] and WithOrderBy4
    [13], [14], [20]. The denominator and passing count drop 3892 → 3888.

## Alternatives considered

- **Reject the Cypher 25 forms to keep the 2024.3 TCK green.** Rejected:
  the acceptance suites carry Neo4j's current behaviour (and two of the four
  scenarios are tagged `@fails:cypher-5`, i.e. deliberately Cypher-25-only);
  reverting would regress the dialect to pre-Cypher-25 semantics.
- **Accept list literals as boolean operands too.** Rejected: openCypher TCK
  Boolean1-3 [8] pins the compile-time error, and only a runtime value can
  take the deprecated conversion.
- **Attach the fulltext deprecation to both query procedures.** Rejected:
  CIP-157's @neo4j-verified inventory of `deprecatedBy` replacements is a
  hard ratchet, and the notification model cannot express the runtime-only
  list/path conversion either; the two acceptance scenarios stay
  notification-missing by design.
- **Give the map projection a source-text column name and rename at the CALL
  boundary only.** Rejected: two naming policies for one expression shape
  (validation vs execution) is exactly the drift the crate tries to avoid;
  naming it after the variable everywhere keeps front and executor agreed.
- **Execute DISJOINT BY batches disjointly.** Rejected for now: the engine
  executes `IN TRANSACTIONS` sequentially, so the hint has no observable
  effect; implementing parallel batches is its own decision and ADR.

## Consequences

- The four superseded TCK scenarios leave the pin (later reversed: ADR-0472
  keeps them in the pin asserting the Cypher 25 behaviour, restoring the
  claim to "3892 of 3892 pinned scenarios pass, 0 skipped";
  `docs/tck/compliance.md` states the current pin).
- The acceptance suite's misc-tail rows parse and run; the three rows that
  expect a deprecation notification (fulltext queryRelationships 01N01,
  list-to-boolean 01N02) remain notification-missing until the engine grows
  runtime notifications.
- `SHOW PROCEDURES` lists the new `db.index.fulltext.queryRelationships`
  row; `docs/reference/cypher-syntax.md` documents it and the visualization
  options.

## Later references

- **ADR-0472 (re-pin of the four ORDER BY scenarios).** Decision 12 is
  superseded: `ReturnOrderBy6 [5]` and `WithOrderBy4 [13]`, `[14]`, `[20]`
  are back in the pinned corpus, allowlisted and asserting Neo4j's current
  Cypher 25 behaviour, so the denominator and passing count are 3892 rather
  than the 3888 this ADR recorded. Every other decision here stands.
