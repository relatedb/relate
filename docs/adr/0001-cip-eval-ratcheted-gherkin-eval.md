---
type: adr
area: cypher
name: 0001-cip-eval-ratcheted-gherkin-eval
status: accepted
tags:
  - conformance
title: "cip-eval: a ratcheted Gherkin eval derived from the CIP documents"
---

# cip-eval: a ratcheted Gherkin eval derived from the CIP documents

To extend CypherLite's Cypher surface beyond the openCypher TCK, we derive an
evaluation suite ("cip-eval") from the 164 Neo4j CIP documents in
`testdata/cip-docs/`. We decided: (1) scenarios are Gherkin `.feature` files
executed by reusing the existing `cypherlite-tck` runtime interpreter, not a
new format or hand-written Rust tests; (2) every CIP is triaged into Tier 1
(language feature — implement), Tier 2 (adapt to embedded context with
documented deviations), or Tier 3 (server/multi-user administration — excluded)
via a human-reviewed manifest, rather than testing all 164 literally or
silently skipping admin docs; (3) expected results are verified against a live
Neo4j (Docker, authoring-time only — never a CI dependency) so a model-written
implementation cannot self-confirm against model-invented expectations; and
(4) QA runs the suite against a checked-in per-CIP baseline that only ratchets
upward — the suite is *designed* to start mostly red, so "green build" is
defined as "no CIP's pass count dropped", not "all scenarios pass".

## Consequences

- cip-eval is excluded from the default `make test` flow; it has its own
  target and CI job so everyday development keeps green-means-green.
- The Gherkin interpreter gains CIP-specific step types (e.g. GQLSTATUS
  assertions) that the upstream TCK does not use.
- Progress is measured by the scoreboard, and regressions are caught by the
  baseline, so partial implementation states are first-class rather than
  branch-blocking.
