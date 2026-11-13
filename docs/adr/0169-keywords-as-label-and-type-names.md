# 0169. Keywords as label and relationship-type names

- **Status**: Accepted
- **Date**: 2026-10-06
- **Deciders**: Oskar Hane (agreed in conversation), CypherLite maintainers (Linear LABS-154)

## Context

LABS-154: Neo4j accepts reserved words as labels and relationship types
without backticks; CypherLite does not. The northwind graph-examples
walkthrough (`northwind-old.adoc`, lines 158/168/192/208) uses `:Order`
throughout, and `MATCH (o:Order) RETURN o` fails with
`syntax error: parse error: expected a label name, got "Order"`. The
graph-examples differential suite (ADR-0144) found it, and
`graph-examples-findings.md` logs it. `cypherlite pull` already works
around it on the write side: `cypher_identifier` in
`surfaces/cypherlite-formatter/src/lib.rs` backticks every word in its
`RESERVED_WORDS` list, and
`surfaces/cypherlite-formatter/src/tests/reserved_words.rs` cross-checks
that list against the parser's `lookup_keyword`. Hand-written queries
still fail.

Root cause: every label/type-name read in
`cypherlite/cypherlite-parser/src/parser/` (the production front-end,
ADR-0046) checks `TokenType::is_ident_like()` in
`cypherlite/cypherlite-parser/src/token.rs`. That predicate accepts
`Identifier` and most keyword tokens (`RETURN`, `NODE`, `COUNT`, `NOT`,
`NULL`, …), so those already work as labels. It deliberately excludes
these 22 keyword tokens:

`MATCH` `OPTIONAL` `CREATE` `INSERT` `MERGE` `DELETE` `DETACH` `SET`
`REMOVE` `WITH` `UNWIND` `WHERE` `ORDER` `BY` `LIMIT` `SKIP` `AS`
`DISTINCT` `FOREACH` `UNION` `REQUIRE` `DROP`

The exclusion exists because `is_ident_like()` also decides variables,
aliases, property keys, map keys and optional variable slots. In those
positions the words would collide with clause and element-pattern syntax.
For example, the optional element variable in `(WHERE …)` or `[WHERE …]`
would read `WHERE` as the variable name.

A Neo4j 2026.09 community docker probe ran all 71 keyword spellings that
`lookup_keyword` recognizes (every spelling that lexes to a keyword token,
e.g. `ASC`/`ASCENDING`, `INFINITY`/`INF`) through 16 query shapes with
`EXPLAIN`. Neo4j accepted all 1136 combinations. The shapes were
`MATCH (n:K) RETURN n`, `MATCH ()-[r:K]->() RETURN r`, `CREATE (n:K)`,
`MERGE (a)-[:K]->(b)`, `MATCH (n) WHERE n:K RETURN n`,
`MATCH (n) WHERE n IS K RETURN n`, `MATCH (n) SET n:K`,
`MATCH (n) REMOVE n:K`, `MATCH (n:A|K) RETURN n`, `MATCH (n:A:K) RETURN n`,
`MATCH (n:K:A) RETURN n`, `MATCH (n:!K) RETURN n`,
`MATCH (n:A&K) RETURN n`, `MATCH ()-[:A|K]->() RETURN 1`,
`RETURN {k: 1} AS m` (map key) and `MATCH (k:A) RETURN k` (variable).
CypherLite gives a syntax error where Neo4j succeeds for:

- the 22 keywords in all 14 label/type shapes
- `NOT` in `n IS K`, because `IS NOT` is parsed as negation
- the same 22 keywords as map keys
- 50 keywords as variables

## Decision

1. **A label-name token predicate.** `token.rs` gains a new
   `TokenType::is_label_name()` predicate. It returns true for
   `Identifier` and for any token where `is_keyword()` is true.
   `is_ident_like()` stays as it is. Every token that `is_ident_like()`
   accepts is either `Identifier` or a keyword, so the new predicate is a
   strict superset of it. No label that parses today stops parsing. The
   name is the token's source literal with its case preserved, so `:Order`
   and `` :`Order` `` name the same label.

2. **Where it applies.** The predicate replaces `is_ident_like()` at every
   label/type-name read site, and only there. These are the positions, all
   in `cypherlite/cypherlite-parser/src/parser/`:

   1. `parse_label_primary` (patterns.rs): the label/type atom of every label
      expression — node `(n:K)`, relationship `[r:K]`, the `IS K` introducer,
      `:A|K`, `:A&K`, `:!K`, parenthesized groups, the `n:K` and
      `n IS [NOT] [LABELED] K` predicates, CASE `WHEN :K` / `WHEN IS K`, and
      DDL `FOR (n:K)` / `FOR ()-[r:K]-()` targets
   2. `parse_colon_chain_segment` (patterns.rs): each bare segment of a legacy
      colon chain (`:A:K`, `:K:A`)
   3. `parse_label_expr_body` (patterns.rs): the `starts_ident_colon_chain`
      lookahead that recognizes a colon chain whose first label is a keyword
      (`:K:A`)
   4. `peek_can_start_label_expr` (patterns.rs): the IS-introducer check that
      recognizes `x IS K` / `x IS NOT K` as a label predicate
   5. `parse_label_set_atom` (clauses.rs): each `SET`/`REMOVE` label item
      (`SET n:K`, `SET n IS K`, `REMOVE n:A:K`)
   6. `parse_using_hints` (clauses.rs): the label in
      `USING INDEX [SEEK] <var>:K(<prop>)`
   7. `parse_using_hints` (clauses.rs): the label in `USING SCAN <var>:K`
   8. `parse_drop` (clauses.rs): the label in legacy
      `DROP INDEX ON :K(<prop>)`
   9. `parse_create_index` (schema.rs): the label in legacy
      `CREATE INDEX ON :K(<prop>, …)`
   10. `parse_create_constraint` (schema.rs): the label in
       `REQUIRE <alias>:K`
   11. `parse_graph_type_ref_inner` (schema.rs): the label after `:`/`IS` in
       graph-type node/edge type specifications and references
       (`(p:K => …)`, `-[:K => …]->`, `CONSTRAINT FOR (p:K)`)
   12. `parse_graph_type_element` (schema.rs): each implied label after
       `=>`/`IMPLIES` (`=> :K&L`)
   13. `looks_like_pattern_comprehension` (expressions.rs): the label-skip
       loop of the pattern-comprehension lookahead (`[(n:K)-->() | …]`)
   14. `is_pattern_predicate` (expressions.rs): the label-skip loop of the
       pattern-predicate lookahead (`WHERE (n:K)-->()`)

   Positions 6, 7 and 10–12 read their label through `parse_bare_name`
   today. They move to a dedicated label-name read that uses the new
   predicate. `parse_bare_name` keeps `is_ident_like()` for its callers that
   read variables, aliases, property names and schema object names.

3. **What stays unchanged.**
   - Keywords as variables (pattern, FOREACH, comprehension and quantifier
     variables), as aliases (`AS`, `YIELD`), as property keys, and as map
     keys or map-projection keys. Neo4j accepts these, but they are
     follow-up work with their own disambiguation.
   - `n IS Not`: `IS NOT` stays the negation prefix. The existing order of
     the IS-suffix branches also stays, so `IS NULL`, `IS TYPED`,
     `IS NORMALIZED` and `IS LABELED` keep their predicate meaning.
   - The diagnostic planner-IR parser (`CYPHERLITE_PARSER=planner-ir`).
     ADR-0277 already removed it from `cypherlite-ir`, so
     `cypherlite-parser` is the only grammar this decision changes.
   - Quoting in `cypherlite-formatter`: `cypher_identifier` keeps
     backticking reserved words.

## Alternatives considered

- **Widen `is_ident_like()` globally to the 22 keywords.** This also
  changes every variable, alias, key and optional-variable position. For
  example, `(WHERE …)` and `[WHERE …]` would read `WHERE` as the element
  variable. Rejected: it changes far more grammar than LABS-154 needs, and
  those positions need their own disambiguation rules and evidence.
- **Lexer-level contextual keywords.** The lexer would emit `Identifier`
  for these words in label context, for example after `:`, `IS`, `|`,
  `&` or `!`. The lexer has no grammar context: the word after `|` is a
  label in `:A|SET` but a clause in `FOREACH (x IN l | SET …)`. Making the
  22 words spelling-matched contextual keywords everywhere instead would
  rewrite every clause dispatch that matches `TokenType::ORDER`, `WHERE`,
  `SET`, and so on. Rejected: it either misclassifies tokens or moves
  parser state into the lexer, for a change that only label positions
  need.
- **Accept only the keyword seen in northwind (`ORDER`).** Rejected: the
  probe shows Neo4j accepts all 22 in every label/type shape. Plausible
  domain labels such as `:Match`, `:Set`, `:Union` or `:Limit` would stay
  known gaps for the next dataset, and the one-word special case still
  needs its own predicate, so it costs the same as the general fix.
- **Make `parse_bare_name` itself accept every keyword.** Rejected: it
  also reads variables, aliases, property names and graph-type and
  constraint names. Widening it would bring the global-widening problems
  into those positions.
- **Keep rejecting keyword labels and document backticks.** Rejected:
  Neo4j-compatible queries such as the northwind walkthrough fail as
  written, and the formatter workaround only covers text that CypherLite
  generates itself.

## Consequences

- The parser change is limited to the predicate in `token.rs` and the 14
  positions listed in the Decision. Each position gets parser tests for
  the 22 keywords, and the northwind `:Order` queries get an engine test.
  The LABS-154 entry in `graph-examples-findings.md` is closed when the fix
  lands.
- The official openCypher TCK must not regress: 3891 of 3893 passing,
  with the two documented skips. The CIP-eval baselines
  (`baselines/cip-eval.json`) must not regress either. If a pinned TCK or
  frozen CIP-eval scenario requires a keyword label to be rejected, a new
  ADR resolves the conflict. A new skip does not.
- Every new position comes after a label introducer or a label-expression
  operator, where the grammar already requires a label or type name. Two
  existing disambiguations still decide the edge cases, so the
  implementation must keep both intact: the comprehension `|` probe
  (`label_or_pipe_is_ambiguous`) and the IS-suffix branch order.
- `cypher_identifier` keeps emitting backticks for reserved words. That
  output stays valid and is preferred for round-trip safety, including
  for older CypherLite versions that reject keyword labels. The
  formatter's `RESERVED_WORDS` cross-check test
  (`test_reserved_word_list_matches_the_parser_keywords`) stays.
- Known gaps remain as follow-up work: keywords as variables and aliases
  (50 keywords in the probe), keywords as map keys (the same 22), and
  `n IS Not`.

## Later references

ADR-0191 makes keywords valid unquoted symbolic names (aliases after
`AS`, variable references, binding variables and map keys); the
variables/aliases/map-key gaps left open here are closed there.

Later change (query-notifications identifier-policy): the formatter now
delegates identifier quoting to
`cypherlite_parser::token::{identifier_needs_quotes, quote_identifier}`
(ADR-0103 R3 permits surfaces → engine crates), so the `RESERVED_WORDS`
mirror and its cross-check test are gone; the backticks-for-keywords
policy above is unchanged.
