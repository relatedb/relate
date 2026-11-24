---
type: adr
area: engine
date: 2026-11-24
deciders: Relate maintainers
claim: 51dca25aa08f89993ee50229c21b879a7846fbdd
name: 0667-ontology-procedure-parity
status: proposed
tags:
  - ontology
  - procedures
title: "`db.ontology.*` procedure contract revision"
REFERENCES:
  - "[[adr/0121-ontology-procedures]]"
  - "[[adr/0226-ontology-mappings]]"
  - "[[adr/0227-ontology-document-format]]"
  - "[[adr/0364-ontology-spec-main-gate]]"
  - "[[adr/0668-ontology-multi-pair-relationship-types-define-no-graph-type]]"
---

# ADR-0667: `db.ontology.*` procedure contract revision

## Context

Relate's `db.ontology.*` record — [ADR-0121](./0121-ontology-procedures.md) with [ADR-0226](./0226-ontology-mappings.md), [ADR-0227](./0227-ontology-document-format.md) and
[ADR-0364](./0364-ontology-spec-main-gate.md) — pinned the family's contract at
an earlier stage of a specification that is still moving. The family is a
preview feature ([ADR-0378](./0378-ontology-procedures-preview-gate.md))
precisely because that contract is not final.

## Decision

The `db.ontology.*` family's contract is revised. The revised contract's
specifics live in the reference documentation
([`docs/reference/ontology-procedures.md`](../reference/ontology-procedures.md))
and in the procedure registry the procedures are implemented against — not in
this ADR. The sections of the record this revision replaces are superseded:
[ADR-0121](./0121-ontology-procedures.md) §3 (transaction rules),
[ADR-0226](./0226-ontology-mappings.md) §2 (the descriptive-field split),
[ADR-0227](./0227-ontology-document-format.md) §2/§3 (the stored form) and
[ADR-0364](./0364-ontology-spec-main-gate.md) §5.4 (the converter's failure
rules). A later ADR records the revised specifics in full once the
specification the family tracks is final, closing [ADR-0378](./0378-ontology-procedures-preview-gate.md)'s removal condition.
