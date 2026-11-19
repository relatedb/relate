---
type: adr
area: surface
claim: 0d39e833aafe934b50605a766343bc2644df36a8
name: 0315-bolt-unknown-message-signature-fatal
status: proposed
tags:
  - bolt
title: |-
  Bolt unknown message signatures are fatal: the connection closes without a FAILURE
REFERENCES:
  - "[[adr/0126-bolt-basic-auth]]"
  - "[[adr/0260-trust-boundary-error-sanitization]]"
  - "[[adr/0314-bolt-protocol-breaches-close]]"
---

# ADR-0315: Bolt unknown message signatures are fatal: the connection closes without a FAILURE

## Context

A Bolt 4.4 request is a PackStream structure whose signature is one of
HELLO `0x01`, GOODBYE `0x02`, RESET `0x0F`, RUN `0x10`, BEGIN `0x11`,
COMMIT `0x12`, ROLLBACK `0x13`, DISCARD `0x2F`, PULL `0x3F` or ROUTE
`0x66`. [ADR-0260](./0260-trust-boundary-error-sanitization.md) item 2 kept the session answering any other signature
with FAILURE `CypherLite.Error` `unknown message tag 0x..` (and a
non-structure with `expected struct`), and continuing.

Latest Neo4j (LABS-323 finding F-31) treats an unknown signature, for
example the Bolt 1-2 ACK_FAILURE `0x0E`, as a fatal decode error. It
sends nothing and closes the connection; the RESET the probe sends next
finds it closed. A client that sends an unknown signature is speaking a
different protocol version, so no reply it could parse exists.

## Decision

1. A message whose structure signature is not a Bolt 4.4 request, or a
   message that is not a structure at all, is a fatal decode error. The
   server sends no FAILURE, logs one stderr line naming the signature,
   and closes the connection at once.
2. The [ADR-0126](./0126-bolt-basic-auth.md) pre-HELLO auth gate keeps precedence: before a validated
   HELLO with auth enabled, any message other than HELLO and GOODBYE
   (an unknown one included) answers the Unauthorized FAILURE and closes,
   as before.
3. The request vocabulary is one typed enum in the session. The signature
 table, Neo4j's message class names ([ADR-0314](./0314-bolt-protocol-breaches-close.md)) and the decode step are
   derived from it, so no second list of tags exists.
4. This reverses [ADR-0260](./0260-trust-boundary-error-sanitization.md) item 2's "the unknown-tag FAILURE is
 unchanged". [ADR-0260](./0260-trust-boundary-error-sanitization.md)'s Later references note is added with the
   failure-code work that also revises that ADR.

## Alternatives considered

- **Keep answering FAILURE and continuing.** Rejected: the client cannot
  be speaking Bolt 4.4, so the session state it assumes is unknown, and
  Neo4j closes.
- **Answer FAILURE, then close.** Rejected: it differs from Neo4j for no
  benefit; a client on another protocol version cannot decode the
  FAILURE anyway.

## Consequences

- A client that sends an unknown signature sees the connection close,
  as with Neo4j.
- Pinned by `tests/compat/scripts/protocol/unknown-message-tag-closes.bolt`
  and the `protocol_breach_tests.rs` unit tests.
