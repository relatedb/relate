---
type: adr
area: engine
claim: e71d4bbe24e1ed058775ea0fd03b7f9a688ea434
date: 2026-11-26
name: 0698-sortable-element-id
status: proposed
tags:
  - perf
  - planner
  - values
title: The element id's entity segment sorts in id order
REFERENCES:
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0555-operators-stream-rows-never-accumulate]]"
  - "[[adr/0554-dash-bearing-element-id-segment]]"
---

# ADR-0698: The element id's entity segment sorts in id order

## Context

An element id is `<kind>:<segment>:<n|r><id>` ([ADR-0554](./0554-dash-bearing-element-id-segment.md)), with the id in plain
decimal: `4:rl-id:n7`, `4:rl-id:n10`. The id is opaque to a client, but a query
sees it as a string, and strings compare character by character. `ORDER BY
elementId(n)` therefore answers `n0, n1, n10, n11, … n19, n2` — not the order
the nodes were created in, and not the order a scan visits them.

Two things follow. A bare node scan already walks ascending ids, so `ORDER BY
id(n)` over it is a no-op the planner can drop; the same sort on `elementId(n)`
is not a no-op, and cannot be dropped. And a client that pages by element id
cannot keyset: `WHERE elementId(n) > $after` selects by the string order, which
skips and repeats nodes, so a window over the whole graph has to page by the
numeric `id()` the element id exists to replace.

Nothing persists an element id: it is emitted from `(id, namespace)` at read
time ([ADR-0554](./0554-dash-bearing-element-id-segment.md)), and the single constructor and decoder live in one module.
The format is therefore a cutover, not a migration.

## Decision

The entity segment is **length-prefixed decimal**: `n` or `r`, then one letter
that names the digit count, then the digits.

- the count letter is `a` for a one-digit id, `b` for two digits, and so on up
  to `s` for nineteen digits (the width of the largest id)
- the digits carry no leading zero, so id 0 is `a0`
- ids `7`, `10`, `100000` are `na7`, `nb10`, `ng100000`; a node is
  `4:rl-id:na7`, a relationship `5:rl-id:ra7`

Within one kind and namespace, the string order of the entity segment is the
numeric order of the id: a shorter id has an earlier count letter, and equal
counts compare digit by digit. Node ids sort before relationship ids because
`4` precedes `5`, the order the kind digit already gives.

The decoder accepts only this form. A non-canonical spelling — the old plain
decimal, a wrong count letter, a leading zero — is malformed, the same answer
as any other malformed id, and the old spelling stops resolving. A negative id
has no element id.

Consequences for the engine, each a separate change:

- `ORDER BY elementId(n)` over a bare node scan is the scan's own order, so the
  planner rewrite that drops `ORDER BY id(n)` extends to it
- a keyset `WHERE elementId(n) > $after` selects the nodes after `$after` in id
  order and can seek, so a window over the whole graph pages by the id the
  client already holds

## Alternatives considered

- **Zero-padded fixed width** (`n0000000000000000007`). Sorts correctly and is
  trivially specified. Rejected: an id is 19 digits wide at the limit, so every
  id on every row grows by 18 bytes, against a segment [ADR-0554](./0554-dash-bearing-element-id-segment.md) shortened for
  wire economy. The length prefix costs one character.
- **Fixed-width hexadecimal.** Rejected: the same width as the padded decimal
  and less readable beside `id()`.
- **Keep the format and order `elementId` numerically inside `ORDER BY`.**
  Rejected: an element id is a string, so `<`, `>` and `=` would still compare
  it as one. Two orders for one value is a silent divergence between the sort
  and the comparison.
- **Keep the format and tell clients to page by `id()`.** Rejected: the element
  id is the identifier the engine hands out, and a window over it cannot be
  expressed.
- **Accept both spellings in the decoder.** Rejected, as in [ADR-0554](./0554-dash-bearing-element-id-segment.md): two spellings
  for one fact is a silent fallback, and an id that resolves by seek but does
  not compare equal as a string would answer differently by plan.

## Consequences

Every client that stored an element id keeps a string the engine no longer
resolves. The engine is pre-release and nothing persists the string, so the
change is made once, now, rather than after clients depend on the old form;
recorded results and fixtures that pin the old spelling are regenerated. A
count letter makes an id look less like a number than the plain decimal did,
which suits an identifier that is meant to be opaque. The encoding fixes a
maximum of nineteen digits; an id wider than that is a broken store and fails
loudly, as the engine's other width limits do.
