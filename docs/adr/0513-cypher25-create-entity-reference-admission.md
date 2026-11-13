# ADR-0513: Cypher 25 CREATE/INSERT entity-reference admission, and the TCK setup re-pin it forces

Status: Proposed
Claim: 37bb8f9563e4f754cb82c5a35ed253dd02a0aa4c

## Context

Cypher 25 makes a write clause's property definitions unable to read an
entity the *same clause* creates. Neo4j reports `42I58` ("invalid entity
reference. Entity, 'a', cannot be created and referenced in the same
clause.") for every spelling:

```
CREATE (a {prop: 7})-[r:R {prop: a.prop}]->(b)   // same pattern, later element
CREATE (a), (b {prop: a.prop})                   // separate patterns, one clause
INSERT (a {prop: 7})-[r:R {prop: a.prop}]->(b)   // the Cypher 25 spelling
```

Live-verified against `neo4j:2026.10.1-enterprise` under `CYPHER 25`
(2026-11-04):

```
$ cypher-shell "CYPHER 25 CREATE (a:End {num: 42, id: 0}), (:End {num: 3}), (:Begin {num: a.id})"
42I58: syntax error or access rule violation - invalid entity reference. Entity, 'a', cannot be created and referenced in the same clause.
  42001: syntax error or access rule violation - invalid syntax
```

CypherLite carried the rule only for the *subquery / pattern-bearing*
spellings (`validate_create_self_reference`, PR #638): a plain property
reference stayed legal because three pinned openCypher 2024.3 TCK
scenarios use the shape as their setup fixture. The harness's
`Create.feature` / `SelfReferenceInCreate.feature` (converted from Neo4j's
own acceptance suite) expects the full rule, so 22 scenarios failed with
`missing-validation`.

The same live probe fixes the second half of the cluster. A CREATE or
MERGE relationship whose node position the row binds to null — the
failed-`OPTIONAL MATCH` idiom — is Neo4j's 22N01 data-exception
`TypeError`:

```
$ cypher-shell "CYPHER 25 MATCH (a {uid:'a'}), (b {uid:'b'})
                 OPTIONAL MATCH (b)-[:LINK_TO]->(c)
                 CREATE (b)-[:LINK_TO]->(a) CREATE (c)-[r:MISSING_C]->(a)"
22N01: data exception - invalid type. Expected the value NULL to be of type NODE, but was of type NULL.
```

CypherLite silently created a fresh anonymous node in that position and
then the relationship. For the MERGE spelling the harness file's header
records a 2026.09.0 observation where MERGE completed and created only the
node; the same queries on 2026.10.1 raise the 22N01 above for both
directions, with `dbms.cypher.lenient_create_relationship=false` (the
default). CypherLite targets 2026.10.x, so the error is the behaviour to
implement; the two converted MERGE scenarios encode the older observation
and cannot pass against it.

## Decision

1. **Reject every read of a same-clause-created entity.** The
   self-reference rule covers plain property reads, function arguments,
   list/map-comprehension bodies, map-projection variable selectors and
   subquery/pattern expressions, in `CREATE` and `INSERT`, for every
   element of the clause (not only the ones already enforced). A
   comprehension-style binder shadows the name it introduces, so
   `CREATE (n {prop: true IN [n IN [false] | n]})` stays legal — the
   "may reference an entity from an earlier clause" family pins it. A name
   bound *before* the clause, or created by an *earlier* clause, is an
   existing entity and stays readable (`CREATE (a {prop: 7}) CREATE (b
   {prop: a.prop})`). The rule remains Cypher-25-only; an explicit
   `CYPHER 5` preamble keeps the 01N00 warning semantics.
2. **A null (or otherwise non-node) binding in a CREATE/MERGE node
   position is 22N01.** It is neither a fresh node (today) nor a lenient
   no-op: the statement fails with Neo4j's message and `TypeError` class.
   This is the write-side companion of ADR-0498's MATCH ruling (a
   null-bound pattern endpoint matches nothing when *reading*; a write
   cannot manufacture the entity the row did not bind).
3. **Re-pin the three openCypher 2024.3 TCK setup queries** that use the
   now-rejected shape. Each is a `having executed:` fixture whose scenario
   asserts WITH/SKIP/LIMIT or join semantics, never the create-dependency
   form; splitting the single multi-pattern CREATE into one CREATE clause
   per pattern produces the identical fixture graph. The scenarios stay in
   the pin, allowlisted and passing — the 2024.3 corpus is not a Cypher 25
   corpus and this is the smallest local deviation that keeps its intent
   (the ADR-0188 / ADR-0472 precedent, under ADR-0002's authority
   ordering: a live-verified current-Neo4j behaviour outranks the 2024.3
   floor).

## Alternatives considered

- **Keep the plain-reference carve-out and leave the 22 harness scenarios
  failing.** Rejected: it would pin Cypher-5 write semantics in a
  Cypher-25-only dialect, and the exact-rejection rule is what the
  harness's Neo4j-converted suite asks for.
- **Delete the three TCK scenarios instead of re-pinning their setup.**
  Rejected: their assertions (WITH … SKIP/LIMIT dependency handling, a
  property forwarded to express a join) are still current Cypher-25
  semantics; only the fixture spelling is superseded. Deleting them would
  shrink real coverage.
- **Recurse the scope-tracking pass into `CALL { }` bodies with their
  import list, so the conservative tier is not needed there.** Rejected:
  it closes one of the three body kinds the pass cannot reach while UNION
  arms and `NEXT` stages stay on the conservative tier, so the two-tier
  contract would remain; and the pass never traverses expressions, so a
  read inside an expression-embedded subquery (`EXISTS`/`COUNT`/`COLLECT`)
  is still decided by the statement fold. One approximation stated once
  beats two reach rules with different gaps.
- **Make MERGE lenient (skip creation on a null endpoint) as the harness
  header's 2026.09 observation suggests.** Rejected: live 2026.10.1 (the
  target) raises 22N01 for both CREATE and MERGE spellings; a no-op would
  encode a superseded version's behaviour and diverge from Neo4j again.
- **Error only in CREATE and keep MERGE's current silent creation.**
  Rejected: MERGE's create path is the same materializer; two policies for
  one shape is the parallel-state class AGENTS.md bans, and 2026.10.1
  answers the same 22N01 for both.
- **Treat a null bound endpoint as an unbound one and create.** Rejected:
  that is today's behaviour, it invents an entity the query never named,
  and Neo4j rejects it.

## Consequences

- **Observable change:** statements that used to succeed now fail with a
  `TypeError`/22N01-class error (a CREATE/MERGE relationship whose endpoint
  the row binds to null or another non-node), and statements that used to
  read a same-clause-created entity now fail with the 42I58-class
  `SyntaxError`. Both are deliberate Cypher-25 admissions; no valid
  Cypher-25 query changes answer.
- **Two knowledge tiers, one rule.** The scope-tracking pass
  (`validate_variable_types`) applies the rule with the exact enclosing
  bindings — top-level statements and `FOREACH` bodies, which share the
  enclosing scope — so an outer-bound bare name is a reference, never a
  fresh entity. The whole-tree battery reaches the nested bodies that pass
  cannot see (`CALL { }`, UNION arms, `NEXT` stages) without their
  correlated outer bindings, so there only unambiguous declarations count:
  a relationship/path variable (CREATE rejects a bound one as
  `VariableAlreadyBound`) or a node the `declares_shape` predicate admits —
  one carrying labels or a property map. The one shape the conservative tier
  misses is a bare declaration inside such a body read by its own clause
  (`CALL { CREATE (a), (b {x: a.x}) RETURN b }` is accepted, and a front
  test pins that acceptance); the shape it must not misfire on —
  `MATCH (c) CALL (c) { CREATE (c)-[:R]->(:X {p: c.q}) }` — stays legal.
  Nested *statement* bodies are read by one clause-ordered fold, so a binder
  inside a body (`[n IN [1] | n]`, `WITH 1 AS n`) shadows exactly as it does
  at the top level.
- **Harness:** 24 of the 27 CREATE-family scenarios pass. Six remain and
  are all stale expectations relative to the target version: one DELETE
  side-effect table that omits `-properties` (Neo4j removes the deleted
  node's property too, so the graph diff is `-nodes 1, -properties 1`),
  three Cypher-5-only create-dependency scenarios the file's own header
  documents as Cypher 5, and the two MERGE scenarios above. They are
  reported for a harness-side re-pin, not worked around here.
- **TCK:** the denominator and allowlist are unchanged (3892); three
  fixture spellings are re-pinned under `testdata/tck/clauses/`.
- **Pins:** front unit tests for the rejected/accepted shapes next to
  `validate_create_self_reference` (`validate/self_reference.rs`), and
  end-to-end 22N01 pins in `cypherlite/tests/write_validation.rs` (CREATE and
  MERGE, start and end positions).

## Later references

ADR-0584 removes Decision 1's CYPHER-5 arm — "The rule remains
Cypher-25-only; an explicit `CYPHER 5` preamble keeps the 01N00 warning
semantics." With CYPHER 25 the only admitted version, a preamble can no
longer select Cypher 5, the stand-down is unreachable, and the
create-self-reference rule applies to every statement. The rest of this
ADR — Decision 1's rule itself, Decision 2, and the two-tier scope
tracking — stands.
