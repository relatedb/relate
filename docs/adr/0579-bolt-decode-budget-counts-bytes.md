# 0579. The Bolt decode budget counts decoded bytes, not values

- **Status**: Proposed
Claim: 7c712f7498badaa8d0812d3f980be34be2add7d5
- **Date**: 2026-11-11
- **Deciders**: the maintainers

## Context

ADR-0082 caps an incoming Bolt message at 262,144 decoded PackStream values
to bound heap amplification: a one-byte scalar on the wire becomes a whole
decoded node. The count is a poor proxy for that memory. A row of three
integer fields is seven values but only a couple of hundred bytes, so the cap
refuses batches of about 37,000 such rows, far below what the wire cap and
the memory the decoder actually uses would allow. Neo4j accepts these
messages, and loaders written for it send batches of well over 100,000 rows.
ADR-0577 made the refusal actionable; this decision removes the arbitrary
threshold.

## Decision

The per-message budget is the size the message decodes to, in bytes. It has
two limits, chosen by whether the session is open: 64 MiB once it is, and
1 MiB before it is (the HELLO and LOGON an unauthenticated peer may send are a
few kilobytes). A peer can open connections freely, so a connection that has
not authenticated yet must not be able to pin the open-session limit; the limit
is read per message, so a HELLO or LOGON that opens the session lifts it for
the rest of its batch. The counting rules below are the same for both:

- Every decoded value is charged one node, which also covers the slot it takes
  in its parent container, so containers, both halves of each map entry and
  struct fields are all counted.
- String and byte payloads are charged their length on top of the node.
- A declared container size whose bare nodes cannot fit the remaining budget is
  refused before any child is decoded, so a hostile header allocates nothing.
- The decoder still reports the budget as its own typed error and the session
  still answers with the failure of ADR-0577; the failure text now states the
  measured size and the limit in MiB (rounded down, since the measured size is
  a lower bound), with a worked example of a row batch; the pre-auth text
  tells the client to authenticate first instead.
- The wire-size cap and the nesting-depth cap are unchanged.

Lists and maps grow by pushing, so their allocation can reach about twice the
charged slots; the worst-case heap for one message is therefore about twice
the limit. The budget is per message and per connection thread, so the
server-wide bound is that figure times the connection cap; before
authentication it is about 2 MiB per connection, not 128 MiB.

## Alternatives considered

- **Keep the count and raise it.** Rejected: any fixed count is arbitrary and
  misprices small rows against large strings.
- **Rely on the wire cap alone.** Rejected: tiny wire values still expand into
  much larger nodes, the amplification the budget exists to bound.
- **Preallocate containers from their declared size.** Rejected: nested
  headers can each declare nearly the whole budget, so the sum of reservations
  is unbounded by it.
- **Remove the intermediate decoded tree.** Not decided here: it removes the
  amplification at its source and would let the budget become generous or go
  away, but is a larger change that can proceed independently.

## Consequences

- Batches of 100,000+ small rows per request decode; a dense message far past
  the old count loads without error.
- The Bolt specification's description of the limit changes from a value count
  to this byte budget.
- A pre-auth message over 1 MiB is refused by the gate it reaches (the
  unauthorized close before HELLO, the limit FAILURE and close in the 5.x
  AUTHENTICATION state) naming the pre-auth limit.
- ADR-0082's counting rule is superseded; its reasoning and the structure of
  the check stand.
