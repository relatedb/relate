---
type: adr
area: cypher
claim: fd027fe981fd76909a534224ba46e8a4c2a9bd5d
date: 2026-11-26
deciders: MemoryDB maintainers
name: 0695-merge-on-create-labels-overwrite-keeps-pattern-labels
status: proposed
tags:
  - dialect
  - semantics
  - write-path
title: MERGE ON CREATE SET labels(n) = keeps the merge pattern's own labels
REFERENCES:
  - "[[adr/0309-bolt-summary-metadata-neo4j-4-4]]"
---

# ADR-0695: MERGE ON CREATE SET labels(n) = keeps the merge pattern's own labels

## Context

The SET clause's dynamic label-write extension spells three operators on
`labels(n)`: `=` makes the node's label list exactly the operand's names,
`+=` adds the operand's names to the ones carried, `-=` drops them — the
`=`-replaces / `+=`-merges grammar property writes follow. In a plain
`SET`, `=` is a true overwrite: an omitted name leaves, an empty list
strips every label.

`MERGE` composes with those items: `ON CREATE SET` / `ON MATCH SET` are
ordinary SET item lists, and the taken arm applies to the row the merge
produced. On the create branch the merge's own pattern gives the fresh
node its birth labels — `MERGE (gem:Person {name: 'Gem'})` creates a node
already carrying `:Person`.

What does `labels(n) = <list>` do on a node the merge just created? A
bare true overwrite strips the pattern's labels, so the node
`MERGE (gem:Person {name: 'Gem'})` created no longer carries `:Person` —
the pattern's own anchor is gone, and re-running the same `MERGE`
creates a second node instead of matching the first. The extension's
documented example for exactly this position expects the additive
outcome — `ON CREATE SET labels(gem) = ['NewLabel']` answering
`['Person', 'NewLabel']`, the same outcome the additive dynamic-label
spelling `SET gem:$(label)` gives — while every other position its
scenarios pin keeps `=` a true overwrite.

## Decision

In the `ON CREATE` arm, `labels(n) = <list>` writes the operand's names
**plus the merge pattern's own labels for that node**: the created node's
label set becomes exactly the pattern's names united with the list's. The
pattern anchors the identity of the node it created; the overwrite
replaces everything else.

1. **The anchor is the node's birth label set** — the labels the merge's
   create path gave it, read once before the arm's items apply. Dynamic
   label atoms in the pattern resolve at creation, so the anchor is the
   resolved set, never the pattern syntax.
2. **Scope is the nodes this merge created.** A target the merge did not
   create — a binding an earlier clause owns — takes the plain true
   overwrite, and the `ON MATCH` arm is untouched: a matched node is not
   a creation, and `=` there stays a full overwrite.
3. **Only the pattern's labels are anchored.** A label added by an
   earlier item in the same `ON CREATE` list is replaced like any other
   non-pattern name, so `=` and `+=` stay distinct operators in the
   position.
4. **`+=` and `-=` are unchanged in every position.**

## Alternatives considered

- **True overwrite everywhere (the previous behavior).** Rejected: it
  strips the merge pattern's own labels from the node the pattern just
  created — breaking the merge's re-run guarantee — and disagrees with
  the one documented example that pins this position; the additive
  outcome that example expects is already what the `:$( )` spelling
  produces in both this engine and Neo4j.
- **`=` additive in the `ON CREATE` arm** (keep every label the node
  carries). Rejected: `=` and `+=` become the same operator in the
  position and the replaces/merges grammar collapses; no pinned scenario
  needs the wider reading — the example's node carries only pattern
  labels.

## Consequences

- The write engine's one overwrite primitive is unchanged — a node's
  labels become exactly the list it is given; the `ON CREATE` arm
  computes the list it passes (pattern names united with the operand's)
  before the primitive sees it.
- The documented example passes with the side effects it states: the
  pattern's labels count once at creation, the operand's new names once
  at the write, and no label the node ends up carrying counts as removed
  (the label-write counters of [ADR-0309](./0309-bolt-summary-metadata-neo4j-4-4.md)).
- The plain-`SET` and `ON MATCH` overwrite pins are untouched; the
  additive dynamic-label spellings stay additive everywhere; relationship
  targets still reject.
- Tests: the write surface's suite in `relate-executor` pins the anchored
  overwrite (pattern labels kept), the unanchored ones (a pre-existing
  target, the `ON MATCH` arm), a no-label pattern, a dynamic label atom
  in the pattern, and an earlier item's label being replaced.
