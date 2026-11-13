# 0191. Keywords as symbolic names (aliases, references, binding variables, map keys)

- **Status**: Accepted
- **Date**: 2026-10-08
- **Deciders**: Oskar Hane (agreed in conversation), CypherLite maintainers (Linear LABS-183)

## Context

LABS-183: Neo4j accepts reserved words as unquoted symbolic names;
CypherLite rejects most. ADR-0169 fixed keyword *labels* and relationship
types and deliberately left keywords as variables, aliases and map keys
as follow-up work -- its "What stays unchanged" list is the work list
here. The graph-examples differential suite (ADR-0144) found it.

Three recommendations walkthrough statements fail as written.
`recommendations.adoc:218` aliases a count as `as` and reads it back in an
expression:

```cypher
OPTIONAL MATCH (m)<-[:ACTED_IN]-(a)-[:ACTED_IN]->(rec)
WITH m, rec, gs, count(a) AS as
OPTIONAL MATCH (m)<-[:DIRECTED]-(d)-[:DIRECTED]->(rec)
WITH m, rec, gs, as, count(d) AS ds
RETURN rec.title AS recommendation, (5*gs)+(3*as)+(4*ds) AS score
ORDER BY score DESC LIMIT 25
```

`recommendations.adoc:268` and `recommendations.adoc:307` alias a list as
`union` and pass it to `size(union)`:

```cypher
WITH m,other,intersection, common, set1, set2,
     set1+[x IN set2 WHERE NOT x IN set1] AS union
RETURN m.title, other.title, common, set1,set2,
       ((1.0*intersection)/size(union)) AS jaccard
```

The suite's report records `expected alias after AS, got "as"` and
`expected alias after AS, got "union"`.

A Neo4j `2026.09.0 Community` docker probe (script
`scripts/keyword_symbolic_name_probe.py`, image `neo4j:2026.09-community`)
ran all 71 keyword spellings `lookup_keyword` recognizes through 31 query
shapes with `EXPLAIN`, on both engines.
`testdata/keyword-symbolic-names/probe.tsv` records 2201 rows, columns
`id group shape keyword neo4j cypherlite query`,
outcomes `accepted` / `syntax_error` / `other_error:<...>`. Neo4j accepts
and CypherLite syntax-rejects 1189 rows:

- **alias 110** rows: `AS <keyword>` after RETURN and WITH items, UNWIND,
  LOAD CSV, and CALL `YIELD <col> AS`;
- **reference 612** rows: a bare RETURN item, the first WITH item, a WITH
  item after a comma, an arithmetic operand, a function and an aggregate
  argument, a WHERE operand, an ORDER BY key, a property-access base, a
  subscript base, a list element, and a comparison operand;
- **binding 357** rows: node, relationship and path variables, FOREACH,
  list and pattern comprehension variables, quantifier and reduce
  variables, and a `CALL (<var>)` import list;
- **key 110** rows: a map literal key, a map projection key, `.prop`
  selector and variable selector, and a pattern property-map key.

`testdata/keyword-symbolic-names/resolutions.tsv` executes the ambiguous
spellings (columns `id query neo4j_result cypherlite_result`) and shows how
each engine actually resolves them: Neo4j parses `RETURN distinct`,
`WITH distinct` and `RETURN not` as variable references and then fails
semantic analysis (`Variable ... not defined`), while CypherLite
syntax-errors; `count(distinct)` fails in Neo4j with "Insufficient
parameters for function 'count'" and in CypherLite with a syntax error;
`RETURN all AS r` returns the variable in Neo4j while CypherLite consumes
`ALL` as the RETURN modifier and syntax-errors; `case`, `count`, `exists`
and `collect` already resolve as variables (the `EXISTS` reference is
lowercased to `exists` today, which is why the probe's uppercase rows
fail); `true`/`false`/`null`/`inf`/`nan` stay literals on both engines;
`MATCH (WHERE 1 = 1)` is an anonymous element with an inline predicate on
both engines while `MATCH (where WHERE true)` fails in CypherLite.

Root cause: `TokenType::is_ident_like()` in
`cypherlite/cypherlite-parser/src/token.rs` excludes the 22
`CLAUSE_KEYWORDS`; several readers are narrower still -- named path and
FOREACH accept `Identifier` only, and the list comprehension and reduce
readers accept `Identifier` plus the 20-keyword `is_schema_ident_token`
set. All four groups read names through those predicates.

The probe also found the other direction: 16 rows where CypherLite accepts
and Neo4j rejects (`over_acceptance`), all literal or set-quantifier
spellings. They stay unchanged and are listed in Consequences.

## Decision

1. **One predicate, no new helper.** `TokenType::is_schema_name()` in
   `token.rs` is `Identifier` or any keyword -- the openCypher
   `SchemaName ::= SymbolicName | ReservedWord` class ADR-0169 introduced
   for label/type names. The alias, reference, binding and key groups all
   read through it; no new token predicate and no new name reader is
   introduced. (`helpers` is empty; the per-group disambiguation below is
   local to each read site.) `parse_bare_name` keeps `is_ident_like()` for
   its USING-hint and DDL callers.

2. **Aliases after `AS`.** The predicate replaces `is_ident_like()` at:
   1. `clauses.rs::parse_return_items` -- the `AS` alias of a RETURN/WITH
      projection item
   2. `clauses.rs::parse_unwind` -- `UNWIND <expr> AS <alias>`
   3. `clauses.rs::parse_load_csv` -- `LOAD CSV ... AS <alias>`
   4. `clauses.rs::parse_call` -- `YIELD <col> AS <alias>`
   5. `schema.rs::parse_show_yield_list` -- `SHOW ... YIELD <col> AS <alias>`

   No keyword has an existing meaning directly after `AS`/`YIELD`, so
   nothing stays behind; all 110 alias rows parse.

3. **Variable references.** The predicate is the fallthrough of:
   1. `expressions.rs::parse_primary` -- the primary-expression variable
      read
   2. `expressions.rs::parse_reduce_expr` -- a bare `REDUCE` (no `(`
      follows) reads as a variable

   The existing primary forms keep priority ahead of the fallthrough:
   `TRUE`/`FALSE`/`NULL`/`INF`/`INFINITY`/`NAN` literals, prefix `NOT`,
   the CASE/COUNT/EXISTS/COLLECT/REDUCE/VECTOR forms, the `ALL`
   quantifier/function, the contextual schema words (`NODE`, `REL`,
   `POINT`, `INDEX`, `SPATIAL`, `FROM`, `OPTIONS`, `CONSTRAINT`,
   `RETURN`), and the `DISTINCT`/`ALL` set quantifiers their callers
   consume. The `EXISTS` bare fallback returns the token's literal
   instead of the hardcoded `exists`, so the spelling round-trips. 592 of
   the 612 rows parse; the 20 that stay are in point 6.

4. **Binding variables.** The predicate replaces the narrower readers at:
   1. `patterns.rs::parse_node_pattern` -- the node pattern variable
   2. `patterns.rs::parse_rel_pattern` -- the relationship pattern variable
   3. `patterns.rs::parse_pattern_path` -- the named path variable
      (`p = ...`)
   4. `patterns.rs::parse_pattern_path_body` -- a subpath variable inside
      a quantified path pattern (`(p2 = (...))*`)
   5. `clauses.rs::parse_foreach` -- the FOREACH loop variable
   6. `clauses.rs::parse_call_scope` -- the `CALL (<var>, ...)` import list
   7. `clauses.rs::parse_let` -- the LET binding variable (already
      `is_schema_name()`; unchanged)
   8. `expressions.rs::parse_quantifier_or_function` -- the quantifier
      variable (`all(x IN ...)`)
   9. `expressions.rs::parse_reduce_expr` -- the reduce accumulator and
      loop variables
   10. `expressions.rs::parse_allreduce_expr` -- the allReduce accumulator
       and loop variables
   11. `expressions.rs::parse_list_literal` -- the list comprehension
       variable
   12. `expressions.rs::try_parse_map_comprehension` -- the map
       comprehension binding variable

   Two existing meanings stay in front of the variable read, both decided
   by a one-token lookahead so that both readings keep working, matching
   Neo4j:
   - **inline element WHERE**: a leading `WHERE` in a node/relationship
     pattern is the variable only when the next token cannot start the
     inline predicate (`)`, `]`, `:`, `IS`, `$`, `-`, `<-`, `WHERE`);
     otherwise the inline-predicate meaning wins. `(WHERE 1 = 1)` and
     `(where WHERE true)` both parse, as does `(WHERE)`.
   - **variable-less GPM label spec**: a leading `IS` is the variable only
     when the next token cannot start a label spec; `(IS)`/`[IS]` read as
     variables while `(IS A&B)`/`-[IS R]->` keep `IS` as the label
     introducer. This is not a divergence: the lookahead keeps both
     readings working, matching Neo4j on both (the probe's `(IS)` row
     parses), so no resolutions row or divergence mark applies.

   All 357 binding rows parse.

5. **Map keys.** The predicate replaces `is_ident_like()` at:
   1. `expressions.rs::parse_map_literal` -- map literal and pattern
      property-map keys (node and relationship property maps both call
      it)
   2. `expressions.rs::parse_map_projection_items` -- the `key: expr`
      entry, the bare variable selector, and the `.prop` selector
   3. `expressions.rs::peek_brace_is_map_projection` -- the lookahead that
      tells a map projection filler from a subquery body after
      EXISTS/COUNT/COLLECT and from a clause body

   All 110 key rows parse.

6. **Existing meanings that keep priority.** Where a keyword already means
   something at a read site, that meaning wins:
   - `TRUE` `FALSE` `NULL` `INF` `INFINITY` `NAN` keep their literal
     meaning (`res-null-ref`, `res-true-ref`, `res-false-ref`,
     `res-inf-ref`, `res-nan-ref`). The 14 over-acceptance rows that read
     a literal as a property/subscript base or an arithmetic/function
     operand stay accepted.
   - prefix `NOT` keeps negation (`res-return-not`). These 10 rows stay
     syntax errors: `ref-return-bare/NOT`, `ref-with-first/NOT`,
     `ref-with-after-comma/NOT`, `ref-function-arg/NOT`,
     `ref-aggregate-arg/NOT`, `ref-where/NOT`, `ref-order-by/NOT`,
     `ref-property-base/NOT`, `ref-list-element/NOT`,
     `ref-comparison/NOT`.
   - `DISTINCT`/`ALL` directly after RETURN/WITH keep the modifier
     meaning (`res-return-distinct`, `res-with-distinct`, `res-all-ref`).
     These 10 rows stay syntax errors: `ref-return-bare/DISTINCT`,
     `ref-with-first/DISTINCT`, `ref-arithmetic/DISTINCT`,
     `ref-property-base/DISTINCT`, `ref-comparison/DISTINCT`,
     `ref-return-bare/ALL`, `ref-with-first/ALL`, `ref-arithmetic/ALL`,
     `ref-property-base/ALL`, `ref-comparison/ALL`.
   - `DISTINCT`/`ALL` inside `fn(` keep the set-quantifier meaning
     (`res-count-distinct`). `ref-function-arg/DISTINCT` and
     `ref-function-arg/ALL` stay accepted.
   - `CASE`/`COUNT`/`EXISTS`/`COLLECT` keep their forms, with the existing
     variable fallback (`res-case-ref`, `res-count-ref`, `res-exists-ref`,
     `res-collect-ref`). No divergence: the fallback is what already makes
     `RETURN case`/`count`/`collect` parse, and the `EXISTS` fallback now
     preserves the spelling.
   - `REDUCE` is not a carve-out: its form only applies when `(` follows,
     so the probe's bare `REDUCE` rows read as variables.

   The deliberate divergences are therefore 36 probe ids: the 20 rows
   above that stay syntax errors, plus the 16 over-acceptance rows listed
   in Consequences. `exception_rows` is exactly those 36 ids.

7. **What stays unchanged.** `cypher_identifier` in
   `surfaces/cypherlite-formatter/src/lib.rs` keeps backticking reserved
   words; the diagnostic planner-IR parser
   (`CYPHERLITE_PARSER=planner-ir`) is untouched; ADR-0169's label and
   relationship-type behavior is untouched; `parse_bare_name` keeps
   `is_ident_like()` for USING hints, DDL property lists and graph-type
   names. The operator spellings `RETURN NOT + 1` and `RETURN NOT[0]`
   stay syntax errors on both engines.

   A follow-up probe of the remaining same-class name positions the
   original 31-shape probe did not reach -- run against the same Neo4j
   `2026.09.0 Community` container, all 71 spellings through each shape
   with `EXPLAIN` -- found every clause keyword accepted at all four:
   the SEARCH binding variable reference (`MATCH (`MATCH`:Doc) SEARCH
   MATCH IN (VECTOR INDEX docs FOR [1.0] LIMIT 2)`), `SCORE AS <var>`,
   `REPORT STATUS AS <var>` (with `ON ERROR CONTINUE`, which Neo4j
   requires) and the SET/REMOVE target variable (`SET MATCH.prop = 1`,
   `SET MATCH:Label`, `SET MATCH = ...`, `SET MATCH += {...}`, `REMOVE
   MATCH.prop`, `REMOVE MATCH:Label`). They therefore read
   `is_schema_name()` too, at `clauses.rs::parse_search_clause` (binding
   reference and SCORE AS variable), `clauses.rs::parse_in_transactions`
   (REPORT STATUS AS), `clauses.rs::parse_set_item` and
   `clauses.rs::parse_remove_item`. Two notes on spellings outside the
   clause-keyword set:

   - The SEARCH binding read's literal-NULL carve-out is dropped: Neo4j
     parses `SEARCH NULL IN (...)` as a variable reference and rejects it
     semantically ("Variable `NULL` not defined" -- exactly what the
     `@neo4j-verified` search binding-variable scenario expects), and
     cypherlite-front's `InvalidSearchVariableReference` check rejects an
     unbound reference at the same layer. A backticked `` `NULL` `` bound
     by the MATCH is accepted by both engines.
   - SET/REMOVE's literal spellings (`TRUE`, `FALSE`, `INF`, `INFINITY`,
     `NAN`) were already accepted by `is_ident_like()` before this
     change; Neo4j parses them as literals and fails its semantic type
     check ("Type mismatch: expected Map, Node, ..."), a check
     CypherLite does not have, so the widening adds no new divergence
     there (same over-acceptance class as the 16 rows in Consequences).

## Alternatives considered

- **Widen `is_ident_like()` itself (drop the `CLAUSE_KEYWORDS`
  exclusion).** This also changes the USING-hint and DDL positions that
  read through `parse_bare_name`, and it would read `WHERE`/`IS` as the
  pattern variable in `(WHERE ...)`/`(IS A&B)`. Rejected: each position
  class needs its own carve-out and evidence, which is what this ADR
  pins down; the positions that should widen get the predicate
  explicitly.
- **Add a shared `is_symbolic_name()` predicate or `parse_symbolic_name()`
  reader.** `is_schema_name()` already has exactly the intended truth
  table and is documented as ADR-0169's superset of `is_ident_like()`.
  Rejected: a second predicate with the same meaning would be a rename,
  and the disambiguations are per-site, not per-predicate.
- **Lexer-level contextual keywords** (emit `Identifier` for keyword
  spellings after `AS`, `.`, `{`, or in a name slot). The lexer has no
  grammar context: the word after `|` is a label in `:A|SET` but a clause
  in `FOREACH (x IN l | SET ...)`, and the word after `{` is a map key in
  `{match: 1}` but a subquery clause in `EXISTS { MATCH ... }`. Rejected
  for the same reasons as ADR-0169's lexer alternative: it either
  misclassifies tokens or moves parser state into the lexer.
- **Backtracking fallbacks for the carve-out rows** (try the modifier or
  prefix reading, retry the word as a variable when it fails). Rejected:
  it would also accept `RETURN NOT + 1` and `RETURN NOT[0]`, which Neo4j
  rejects (probe rows `ref-arithmetic/NOT`, `ref-subscript-base/NOT`),
  creating new over-acceptance, and it would turn the agreed divergences
  (`RETURN distinct`, `RETURN not`) from syntax errors into semantic
  errors with a different error class. The resolutions fixture records
  the current outcome as the decision.
- **Cover only the spellings the recommendations walkthrough uses
  (`as`, `union`).** Rejected: the probe shows the gap is whole position
  classes across all 71 spellings; a one-spelling special case costs the
  same predicate change and leaves every other spelling a known gap, so
  the next dataset finds the same bug.
- **Fix only aliases and references, leave binding and keys.** 467 of the
  1189 gap rows are binding or key positions. Rejected: the probe's four
  groups are one dialect rule (keywords are symbolic names where
  unambiguous), and a partial fix leaves the fixture failing in three of
  the four groups.

## Consequences

- 1169 of the 1189 gap rows now accept (alias 110, reference 592,
  binding 357, key 110); the 20 rows listed in Decision point 6 stay
  syntax errors by decision. The 16 over-acceptance rows stay accepted
  and unchanged: `ref-arithmetic/FALSE`, `ref-arithmetic/TRUE`,
  `ref-function-arg/ALL`, `ref-function-arg/DISTINCT`,
  `ref-function-arg/FALSE`, `ref-function-arg/TRUE`,
  `ref-property-base/FALSE`, `ref-property-base/INF`,
  `ref-property-base/INFINITY`, `ref-property-base/NAN`,
  `ref-property-base/TRUE`, `ref-subscript-base/FALSE`,
  `ref-subscript-base/INF`, `ref-subscript-base/INFINITY`,
  `ref-subscript-base/NAN`, `ref-subscript-base/TRUE`. The probe fixture
  is the acceptance test: every gap row accepts except `exception_rows`,
  and every `exception_rows` id keeps its recorded outcome.
- The official openCypher TCK must not regress: 3892 of 3892 pinned
  scenarios passing with zero skips (the superseded
  `TypeConversion4 [10]` outline is removed from the pin itself, ADR-0188).
  The CIP-eval baselines (`baselines/cip-eval.json`) must not regress
  either. If a pinned TCK or frozen CIP-eval scenario requires a keyword
  name to be rejected, a new ADR resolves the conflict. A new skip does
  not.
- One acceptance change goes beyond the probe's gap rows:
  `[true IN [1]]` (no `|`) becomes a list comprehension and returns
  `[1]` instead of a list literal containing an `IN` expression
  (`[false]`). That matches Neo4j `2026.09.0` (checked against the probe
  container image); the TCK gate above is the check.
- Honesty about the two error directions: where Neo4j parses the word as
  a variable and then fails semantic analysis (`RETURN distinct`,
  `WITH distinct`, `RETURN not`, `count(distinct)`), CypherLite keeps a
  syntax error; where the modifier meaning wins inside `fn(`, CypherLite
  accepts more than Neo4j (`abs(DISTINCT)`, `abs(ALL)`). Both are
  `exception_rows`.
- `cypher_identifier` keeps emitting backticks for reserved words. That
  output stays valid and is preferred for round-trip safety, including
  for older CypherLite versions that reject keyword names. The
  formatter's `RESERVED_WORDS` cross-check test
  (`test_reserved_word_list_matches_the_parser_keywords`) stays.
- The three recommendations statements parse as written, so the
  graph-examples `expected alias after AS` failures are closed when the
  fix lands. The clause keywords stay in `CLAUSE_KEYWORDS`; the widened
  predicate remains opt-in per position.
- The pattern-comprehension gate in `parse_list_literal` reads the real
  grammar. For the `p = ...` form, the parser speculatively runs
  `parse_pattern_path` after the `=` (token position and the clause-level
  `label_saw_*`/`quantifier_saw_*` side-effect flags snapshotted and
  restored around the probe, so a failed probe leaves no parser state
  behind and cannot trip the clause-end mixing checks) and keeps the
  pattern-comprehension reading only when `|` or `WHERE` follows the
  pattern; otherwise the list-literal reading wins. Every pattern shape
  `parse_pattern_path` accepts is therefore recognized after `p =`:
  label-expression and GPM bodies (`[p = (n:A|B)-->() | p]`,
  `[p = (n:!A)-->() | p]`, `[p = (n:A&B)-->() | p]`, `[p = (n:%)-->() | p]`,
  `[p = (n:(A|B))-->() | p]`, `[p = (n:$(x))-->() | p]`,
  `[p = (IS A&B)-->() | p]`), bare-node and juxtaposed bodies
  (`[p = (n) | p]`, `[p = (a)(b) | p]`), a parenthesized subpath
  (`[p = ((q = (a)-->(b))) | p]`) and the `shortestPath(...)` wrapper
  (`[p = shortestPath(((a)-->(b))) | p]`). `[true = x]`, `[null = x]`,
  `[true = (1 = 1)]`, `[true = false]` and `[x = 1]` stay list literals,
  matching Neo4j, where a pattern never follows a literal. The no-`p` form
  is unchanged: it keeps the `node_pattern_precedes_relationship`
  lookahead shared with pattern predicates (a node pattern immediately
  followed by `-`/`<-`), so `[(n)-->() | n]` is a pattern comprehension
  while bodies without a relationship (`[(n) | n]`) or with a label
  expression (`[(n:A|B)-->() | n]`) stay syntax errors.

## Later references

Later change (query-notifications identifier-policy): the formatter now
delegates identifier quoting to
`cypherlite_parser::token::{identifier_needs_quotes, quote_identifier}`
(ADR-0103 R3 permits surfaces → engine crates), so the `RESERVED_WORDS`
mirror and its cross-check test are gone; the backticks-for-keywords
policy above is unchanged.
