---
type: adr
area: cypher
claim: 95313d270d278540a725617f6f5f95853a2aae8f
date: 2026-11-24
name: 0679-match-mode-bindings-spellings
status: proposed
tags:
  - dialect
title: |-
  The BINDINGS spellings of the match modes join the dialect
---

# 0679: The BINDINGS spellings of the match modes join the dialect

## Context

The match-mode prefix (`MATCH REPEATABLE ELEMENTS`, `MATCH DIFFERENT
RELATIONSHIPS`, each also in the singular) has two more spellings in the
acceptance corpus's match-mode suite: `REPEATABLE ELEMENT BINDINGS` and
`DIFFERENT RELATIONSHIP BINDINGS`. Today the two-word phrase parser stops
after the second word, so the third reads as a pattern start and the
statement fails to parse.

The corpus pins the two spellings only against their element twins'
behaviour: anonymous relationship hops may repeat within a path under the
REPEATABLE form and may not under the DIFFERENT form, in the same clause
and across clauses of one statement. No runnable row pins the two
spellings' behaviour for *repeated relationship variables* — the
equijoin-under-REPEATABLE rows and the no-solutions warning rows cover
only the element forms.

## Decision

The phrase table accepts an optional third word: `REPEATABLE ELEMENT
[BINDINGS]` and `DIFFERENT RELATIONSHIP[S] [BINDINGS]` parse as match
modes, mapped at the parse site onto their element twins' existing
`MatchMode` variants — `REPEATABLE ELEMENT BINDINGS` is the
`RepeatableElements` mode, `DIFFERENT RELATIONSHIP BINDINGS` the
`DifferentRelationships` mode. Every rule that reads a match mode (the
relationship-uniqueness pool, the path-mode combination check, the
unbounded quantified-pattern rejection, the per-clause mode
independence) follows the twin by construction: the mode *is* the twin's
value.

The finer distinction the standard's binding-scoped names carry —
whether repeated relationship variables equijoin or must differ — is not
observable in any runnable corpus row; it is deliberately not modelled
here. A later corpus sync that pins it lands it as its own change, at
which point the variants split and the binding-scoped pool appears.

## Alternatives considered

- **Two new enum variants mapping to the twins' policies.** Rejected:
  every match-mode reader would carry a second arm that must agree with
  its twin's — parallel state with no observable contract behind the
  split, exactly the shape the code-shape rules reject.
- **Model a binding-scoped uniqueness dimension now.** Rejected: no
  runnable row distinguishes it from the element forms.
- **Parse the spellings and reject them semantically.** Rejected: the
  corpus executes them and asserts results, so a semantic rejection
  trades four honest rows for an invented error.
- **Treat the third word as a pattern-start error (status quo).**
  Rejected: that is the parse gap the corpus exposed.

## Consequences

- The parser's match-mode phrase table accepts the optional third word
  in every position it already recognizes or rejects the phrase; the
  AST and IR `MatchMode` enums are unchanged.
- The acceptance corpus's match-mode suite rows for the two spellings
  turn green; the element forms' rows are unchanged.
