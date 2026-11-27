---
type: adr
area: cypher
claim: 3195c8b0376edb201527690bb608780298f2b60f
date: 2026-11-27
deciders: Relate maintainers
name: 0711-in-non-list-operand-is-one-element
status: proposed
tags:
  - dialect
  - expressions
title: IN's non-list right operand answers as the one-element list holding it
REFERENCES:
  - "[[adr/0566-compile-time-static-rejections-are-syntax-errors]]"
---

# ADR-0711: IN's non-list right operand answers as the one-element list holding it

## Context

The spec harness's first run of a previously-skipped acceptance row
asserts `WHERE movie.rating IN $ratings` with `$ratings = 7.6` returns
the same row the list form does — the scenario's own name is "parameter
is not a list, but it works as one". The engine answered the typed
rejection `IN requires a list on the right-hand side`.

Three neighbouring rules already exist and must not move:

- the compile-time literal form (`1 IN 2`) stays the front's static
  rejection ([ADR-0566](./0566-compile-time-static-rejections-are-syntax-errors.md));
- the SEARCH filter forms keep their own prepare-time list rule — the
  vector-search window's property-in-set and entity-set filters reject a
  non-list right operand before evaluation, and the corpus pins that
  rejection;
- a null right operand stays null (`x IN null` is null).

## Decision

The runtime membership operator evaluates a **scalar** right operand —
a boolean, number, string or temporal, the one membership-scalar
kind-set the search filters' member policy already stated — as the
one-element list holding it: `x IN 7` is `x IN [7]`. The wrap routes
through the one membership policy, so null-member and equality rules
are exactly the list form's; the fold is `cypher_eq` itself, nothing
allocates. A **container that is not a list** — a map, a vector, an
entity, a path, a point — stays the typed rejection the list rule
always was; the corpus pins the vector form (IN membership against a
vector is a type error) and the map form (a map right operand in a map
comprehension). Only the general operator changes — the two SEARCH
consumers keep their separate rules (their prepare-time gate stands
ahead of the shared operator).

## Consequences

- Both corpus rows pass: the scalar-parameter row through the
  one-element fold, the vector and map rows through the retained
  rejection; the old rejection text stays for the container forms.
- `x IN <scalar>` answers false (not null) when the operands differ —
  a one-element list with no null member has no unknown case.
- The compile-time literal rejection and the SEARCH filter rejections
  are untouched, so no pinned behaviour moves with this change.
