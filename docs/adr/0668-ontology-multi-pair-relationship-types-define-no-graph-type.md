---
type: adr
area: engine
claim: 1598f100517b5b91f0646b3a09f084d8f21e21c8
date: 2026-11-23
deciders: the maintainers
name: 0668-ontology-multi-pair-relationship-types-define-no-graph-type
status: proposed
tags:
  - engine
  - ontology
title: "Ontology relationship types declared across several endpoint pairs define no graph type: the mapping stays narrow, the type stays writable"
REFERENCES:
  - "[[adr/0226-ontology-mappings]]"
  - "[[adr/0649-vault-committed-ontology-file]]"
  - "[[adr/0487-ontology-spec-1-1-0-pin]]"
---

# 0668: Ontology relationship types declared across several endpoint pairs define no graph type

## Context

The Ontology Graph Spec (ADR-0487's 1.1.0 pin) lets one relationship
`type` appear in several `relationships` entries, each with its own
`from`/`to` node pair — a real vocabulary shape: a memory vault wires
`PART_OF` across `(LedgerRecord|Todo|Source) -> (Immutable|Source|Draft)`
as five declared pairs. ADR-0226 §2 maps every entry to its own edge
type spec and records a caveat: "graph-type enforcement keys on the type
alone." The caveat's consequence was never worked through, and it fails
on both halves:

- **`db.ontology.apply` refuses the document.** The converted body
  carries several edge type specs with the same identifying label, and
  the shared `ALTER CURRENT GRAPH TYPE SET` pipeline validates it with
  the graph-type body validator's duplicate-identifying-label rule — a
  spec-valid document dies with a generic syntax error before any data
  check.
- **The vault-committed file (ADR-0649) installs it and then enforces
  it impossibly.** The open tail replays the converted spec without the
  body validation, so the store opens — but the graph type's derived
  endpoint rows (`sourceLabel`/`targetLabel`, one per entry **side**)
  are enforced independently, so every relationship of the type must
  carry **every** entry's start label and **every** entry's end label.
  A type declared across two pairs admits no writable pair at all: each
  declared write fails with `endpoint label presence verification
  failed: expected <the other entry's label>`.

## Decision

1. **A relationship type declared across several endpoint pairs defines
   no graph type at all.** The converter emits no edge type spec for
   it, so no endpoint, property or constraint rows derive and its
   relationships stay writable under any labels. The type still seeds
   the load's vocabulary (map-form link entries under it are declared,
   ADR-0649 §2), the document converts and `db.ontology.apply` accepts
   it, and the vault open tail installs nothing for it — both reported
   failure modes are gone. Endpoint enforcement for such a type is
   forgone by design: the vocabulary shape wins over the constraint.
2. **A type declared once keeps exactly today's semantics.** One entry
   — or entries restating one identical resolved pair — compiles to one
   spec, and its endpoint, property and constraint rows enforce as
   before, byte-identical.
3. **Restatements are resolved, not guessed.** An entry restating an
   earlier entry's resolved pair (the label pair — two node keys of one
   identifying label are the same endpoint) identically compiles once;
   one restating it with a different payload is a loud `Mapping`
   failure naming both entries — a genuine document conflict, never a
   silent first-wins. The policy polices the types that define a graph
   type: the multi-pair verdict is decided from the entries' resolved
   endpoints before any payload compiles, so a skipped type's
   declarations — a token the body cannot express, a restated
   declaration — never refuse the document, through `db.ontology.apply`
   and the vault open alike.
4. **One label, one node type.** Node entries sharing an identifying
   label are refused at the mapping with a named diagnostic — a node
   type's identity is its label, so two entries declaring one label are
   a duplicate the mapping refuses by name instead of the validator's
   late, generic duplicate error.
5. **No validation-scope change.** Converted bodies never carry
   same-type edge specs, so they validate under the ordinary alteration
   rules; the grammar's duplicate-identifying-label rule stands
   everywhere, for grammar writers and the mapping alike.

No storage change and no executor change: the enforcement machinery for
single-declared types is exactly what main already runs.

## Consequences

- `db.ontology.apply` accepts multi-pair documents; the vault open tail
  installs them as vocabulary; every relationship of a multi-pair type
  is writable; `SHOW CURRENT GRAPH TYPE` and `SHOW CONSTRAINTS` simply
  carry nothing for the type.
- A vault that wants endpoint enforcement for a type declares that type
  once; a type that legitimately spans pairs is declared for the
  vocabulary and enforced nowhere. A later ADR may add per-pair
  enforcement if real vaults need it — the pair structure is preserved
  in the document either way.
- ADR-0226 §2's caveat line is resolved by this ADR: the mapping no
  longer emits same-type specs at all.

## Alternatives considered

- **Per-entry pair enforcement** (a relationship valid against any
  declared pair, the pair set read from the current graph type). Built
  first for this change and withdrawn on review: it needs the pair set
  as a second constraint channel beside the catalog's per-side rows, a
  validation-scope exemption so converted bodies can carry same-type
  specs, and a verdict memo to keep wide types cheap — widening three
  layers to express what one skipped spec expresses by silence.
- **Refuse multi-pair declarations up front.** Rejected: the Ontology
  Graph Spec allows the shape and real vocabularies need it; refusing
  would push users to undeclare their real vocabulary.
- **Merge same-type entries into one spec.** Rejected in ADR-0226
  already: one spec cannot express label alternation at an endpoint,
  and the merge would silently pick one pair.
- **Enforce per side but disjunctively** (start matches any declared
  start AND end matches any declared end). Rejected: it admits the
  undeclared cross pairs — the pair, not the side, is the declared
  unit.
