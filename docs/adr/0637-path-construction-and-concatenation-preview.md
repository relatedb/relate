---
type: adr
area: cypher
claim: c71fc14511315657532d8f98af3cef7ba0f6267e
date: 2026-11-20
deciders: MemoryDB maintainers
name: 0637-path-construction-and-concatenation-preview
status: proposed
tags:
  - cypher
title: Path values: PATH [ ... ] construction and path || concatenation behind preview gates
REFERENCES:
  - "[[adr/0371-preview-feature-gating]]"
---

# 0637: Path values: PATH [ ... ] construction and path || concatenation behind preview gates

Status: Proposed

Claim: c71fc14511315657532d8f98af3cef7ba0f6267e

## Context

Cypher can only obtain a path by matching one. Two accepted proposals add
path values built from parts: a constructor, `PATH [n0, r1, n1, ...]`, and
a concatenation operator, `p1 || p2`. Both extend syntax or operator
typing, and neither is in the pinned dialect floor, so
[ADR-0371](./0371-preview-feature-gating.md) applies: they ship gated.
Neither proposal fixes every edge case; the choices below are assumptions
for the maintainers to rule on, not settled contract.

## Decision

Two registry rows in the `RELATE_PREVIEW` table: `path-construction` gates
`PATH [ ... ]`; `path-concatenation` gates `||` on path operands. While a
`path-construction` is off the constructor fails with the authored preview
error; while `path-concatenation` is off, `||` on paths keeps the existing
`||` operand type error, since it gates operator typing and an authored
error would advertise a path form the operator rejects. `||` on strings and
lists is unchanged and never gated.

The gate locations sit beside the ADR-0497 exceptions: the constructor gates
at the parse point, and the operator at evaluation, not at a shared
registry dispatch. The parser recognises the constructor at the token level: a `PATH`
identifier token directly followed by a left bracket, peeked before the
primary is consumed, so no variable name is compared after parsing.

Assumptions for the maintainers to rule on:

1. **Single node.** `PATH [n]` is not constructible: it reads as `path[n]`.
2. **Relationship direction.** `PATH [a, r, b]` accepts `r` in either
   stored direction and keeps it; adjacency is checked on endpoints.
3. **Non-alternating list.** Anything but node, relationship, node, ...
   starting and ending with a node is an invalid-argument-type error.
4. **Non-adjacent relationship.** A relationship not connecting its
   neighbouring nodes is an error.
5. **Null elements.** Any null element or a null list gives null.
6. **Boundary node.** `p1 || p2` requires the last node of `p1` to be the
   same node (by id) as the first of `p2`, shared once; else an error.
7. **Null operand.** `path || null` and `null || path` give null; any other
   non-path operand is an invalid-argument-type error.

## Alternatives considered

- **Ungated.** Rejected: both forms are outside the pinned floor.
- **One gate for both.** Rejected: the constructor and the operator are
  independent proposals and can graduate separately.
- **Coerce malformed lists or silently drop null elements.** Rejected:
  hides query bugs, against the no-silent-fallback rule.
- **Require the relationship to match a stated direction.** Rejected: a
  path carries no per-hop direction to check it against.

## Consequences

- Users opt in with `RELATE_PREVIEW=path-construction` and/or
  `path-concatenation`; a path built from parts has the same value shape as
  a matched path.
- Each numbered assumption stays open until the maintainers accept or
  amend this ADR; a changed ruling is a new ADR plus a **Later
  references** note here.
