# 0119. CIP-eval drops frozen scenarios that need Enterprise databases

- **Status**: Accepted
- **Date**: 2026-09-27
- **Deciders**: CypherLite maintainers (Linear LABS-102)

## Context

ADR-0047 §1 requires the product to pass every CIP-eval scenario that the
original engine passed. The frozen scenario identities live in
`testdata/cip-eval/ENGINE_PASSING.json` (ADR-0052). CI fails when a frozen
identity stops passing, and when a CIP's pass count drops below
`baselines/cip-eval.json` (`testing/cypherlite-cip-eval/tests/cip_eval.rs`).

Some frozen scenarios pass only because database administration was metadata
and `USE` was ignored. ADR-0116 adopts Neo4j Community Edition's database
semantics, under which Neo4j itself rejects these scenarios:

- The `cypher-versions-default-language` files `01` to `03` create, replace,
  alter and drop their own databases. Every scenario's setup runs
  `DROP DATABASE … IF EXISTS`, which CE rejects with 51N27, and several
  scenarios create composite databases.
- Files `04` (scenario 3) and `05` (scenario 2) `USE` fixture databases that
  the scenario never creates. `07` (scenario 3) uses a composite constituent.
- The `call-variable-scope-clause` suite's `feature-interactions` scenarios 4
  to 6 call `USE graph.byName(g)` on a standard database, which Neo4j rejects
  (42N72). They were derived from the proposal document, not verified against
  a live server.

The other frozen database scenarios — the ones that filter by name or target
`neo4j` and `system` — keep passing under ADR-0116.

## Decision

1. Remove exactly these 24 identities from `ENGINE_PASSING.json`:
   - `cypher-versions-default-language/01-create-database.feature#0`
     to `#8` (9 identities);
   - `cypher-versions-default-language/02-alter-database.feature#0`
     to `#5` (6);
   - `cypher-versions-default-language/03-show-databases.feature#0`
     to `#2` (3);
   - `cypher-versions-default-language/04-effective-language-aaa.feature#2`,
     `cypher-versions-default-language/05-effective-language-bbb.feature#1`
     and
     `cypher-versions-default-language/07-effective-language-composite.feature#2`
     (3);
   - `call-variable-scope-clause/feature-interactions.feature#3`, `#4`
     and `#5` (3).
2. In the same change, lower `baselines/cip-eval.json` to 9 of 30 for
   `cypher-versions-default-language` and 34 of 38 for
   `call-variable-scope-clause`. The ratchet continues upward from there.
3. Keep the feature files unchanged. They record verified Neo4j Enterprise
   behaviour, and rewriting them to CE semantics would falsify their
   provenance. They stay in the suite as failing scenarios.
4. Update the `testdata/cip-eval/MANIFEST.md` rationales that say CypherLite
   has no system database or no multi-database catalog (the affected
   database-catalog and graph-reference suites). Record
   `cypher-versions-default-language` as supported only for
   `SET DEFAULT LANGUAGE` on the fixed namespaces.
5. This supersedes ADR-0047 §1 for these 24 identities only. Every other
   frozen identity remains a hard gate.

## Alternatives considered

- **Keep them green with lazy `USE` resolution and a no-op
  `DROP DATABASE … IF EXISTS`.** Rejected: both diverge from CE, and 21
  scenarios would still fail because their setup creates or alters databases.
- **Add setup steps that create the fixture databases.** Rejected: under
  ADR-0116, `CREATE DATABASE` itself fails.
- **Edit the scenarios to expect CE errors.** Rejected: it would change what a
  Neo4j-verified fixture claims about Neo4j.
- **Keep the database theater for CIP-eval only.** Rejected: a test-only
  engine mode would hide that the product surface changed.

## Consequences

- CI passes with the reduced frozen set and baseline, and any other regression
  still fails it.
- The `cypher-versions-default-language` and `call-variable-scope-clause`
  scores drop. Release notes explain that multi-database administration is
  not part of CypherLite.
- ADR-0047 gains a Later references note pointing here.
