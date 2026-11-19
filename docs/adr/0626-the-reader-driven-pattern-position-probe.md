---
type: adr
area: cypher
claim: 8a711bb25737a59c6a65cd4615417d08d68aa64f
name: 0626-the-reader-driven-pattern-position-probe
status: proposed
tags:
  - dialect
  - parser
title: The reader-driven pattern-position probe
REFERENCES:
  - "[[adr/0191-keywords-as-symbolic-names]]"
---

# ADR-0626: The reader-driven pattern-position probe

## Context

The front end decides whether a leading `(` in an expression starts a
pattern (a `WHERE` pattern predicate, or a pattern comprehension after
`[`) or a parenthesized expression. ADR-0191 gave that decision to a
token-level lookahead: a node pattern immediately followed by `-`/`<-`.
The lookahead hand-mirrored the node-pattern grammar — variable, labels,
property map, inline `WHERE` — and drifted from the reader twice: the
inline-`WHERE` scan first, then the label grammar, whose hand-rolled copy
recognized only simple colon-chains. Reader-accepted forms — `:A|B`,
`:A&B`, `:!A`, `:%`, parenthesized groups, dynamic atoms, `IS`-introduced
label specs, the `$param` whole-map property shorthand — fell to the
expression reading and died on the relationship token, splitting the
dialect: MATCH accepted node patterns the predicate and comprehension
positions rejected. The reference server plans all of them.

## Decision

The lookahead runs the real reader. `parse_node_pattern` executes
speculatively under save/restore of the token position and the
clause-level side-effect flags — the probe shape the `p = …`
pattern-comprehension gate already uses — and the verdict is a successful
probe followed by `-` or `<-`. The node-pattern forms recognized in
pattern positions are the reader's by construction; no hand-mirrored
grammar remains to drift.

## Alternatives considered

- Extend the hand-mirror arm by arm (add the label-expression extent
  scanner, then the `IS` introducer, then the `$param` shorthand) —
  rejected: every arm left hand-derived is a fresh drift site; this
  change is the mirror's second repair and a third followed within one
  review round.
- Delegate only the label part to the reader's extent scanner and keep
  the remaining arms hand-mirrored — rejected for the same reason: the
  property and inline-`WHERE` arms were the next drift sites.

## Consequences

- A pattern predicate and a pattern comprehension's leading node admit
  every node-pattern form the reader accepts; `WHERE (a:A|B)<-[:R]-(b)`
  and `[(x:A&B)-[:R]->(y) | y]` parse, plan and evaluate like their MATCH
  counterparts.
- ADR-0191's recorded consequence — that the no-`p` comprehension form
  keeps label-expression bodies as syntax errors — is reversed; its
  Later references notes this ADR.
- A failed probe leaves no parser state behind (position and side-effect
  flags restore exactly), so clause-end mixing checks see only the
  committed parse.
- Rejected inputs keep authored errors: a `(` the reader cannot parse, or
  a node pattern not followed by `-`/`<-`, still reads as an expression
  and fails there.
