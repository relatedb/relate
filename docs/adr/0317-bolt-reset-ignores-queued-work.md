---
type: adr
area: surface
claim: e09311af2e90b7ad91f4ddf700db137018797337
name: 0317-bolt-reset-ignores-queued-work
status: proposed
tags:
  - bolt
title: Bolt RESET answers IGNORED for work queued ahead of it
REFERENCES:
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0126-bolt-basic-auth]]"
  - "[[adr/0315-bolt-unknown-message-signature-fatal]]"
  - "[[adr/0316-bolt-client-noop-chunks]]"
---

# ADR-0317: Bolt RESET answers IGNORED for work queued ahead of it

## Context

Bolt drivers pipeline. A driver that gives up on a query sends RESET
right behind the RUN and PULL it already wrote, and expects RESET to
interrupt that work. Neo4j's network layer sees the RESET as soon as it
is decoded and marks the session interrupted: every message queued ahead
of it that has not started answers IGNORED, then RESET answers SUCCESS.

cypherlite-bolt reads one message at a time and runs each to completion
before reading the next, so it never sees a RESET early. The LABS-323
probe (finding F-27) sent RUN, PULL, RESET, RUN, PULL in one write:
latest Neo4j answered the PULL IGNORED (and raced on the first RUN),
cypherlite-bolt streamed all three records first.

## Decision

1. The server frames every complete message already received before it
   handles the next one. It reads from the socket into a connection
   buffer, and one read returns whatever has arrived; each batch is the
   complete messages in the buffer at that point.
2. When a batch contains a RESET, each message queued ahead of the last
   RESET in the batch that has not started answers IGNORED (no RECORDs).
   RESETs in that range are answered normally (SUCCESS, rolling back an
   open transaction and clearing the failed state and open results), and
   the messages after the last RESET run normally. For one write of
   `RUN, PULL, RESET, RUN, PULL` the replies are IGNORED, IGNORED,
   SUCCESS, then the second RUN's SUCCESS, RECORD and SUCCESS.
3. HELLO and GOODBYE are never interrupted. The [ADR-0126](./0126-bolt-basic-auth.md) pre-HELLO auth
 gate and the [ADR-0315](./0315-bolt-unknown-message-signature-fatal.md) fatal decode keep precedence.
4. A request already executing when RESET arrives is not interrupted:
   the connection thread runs it and has no concurrent reader.
5. The batched framing keeps the 16 MiB message cap (a chunk header that
   would cross it fails before its body is read), the per-chunk idle
 deadline re-arm ([ADR-0110](./0110-bolt-connection-query-governance.md)), and NOOP skipping ([ADR-0316](./0316-bolt-client-noop-chunks.md)).

## Alternatives considered

- **A reader thread that watches for RESET while a request runs.**
  Rejected: it needs a second owner for the socket and the session state,
  and a running request still could not stop at a safe point without
  engine cancellation. Queued work is what drivers interrupt in practice.
- **Answer the first RUN SUCCESS and only ignore the PULL**, as Neo4j
  sometimes does. Rejected: Neo4j races on that RUN; always IGNORED is
  within its behaviour and deterministic.
- **Peek the socket for more bytes before each message.** Rejected: the
  stream is generic (raw TCP, TLS, WebSocket), and a peek would cost a
  syscall per message.

## Consequences

- Drivers that reset a pipelined query get IGNORED for it instead of its
  records, as with Neo4j.
- The server keeps a per-connection read buffer, bounded at the 16 MiB
  message cap plus one 64 KiB read. A client that keeps a message
  incomplete while streaming past the bound — pre-HELLO included, since
  framing runs before the auth gate — fails the connection with an
  `InvalidData` error instead of growing the buffer without bound.
- Framing a partial message resumes at its last parsed chunk: the scan
  cursor, the accumulated payload length, and the body ranges are carried
  on the framer across reads, rebased when the buffer drains, so a
  trickled message is parsed once per new byte, not re-parsed from its
  start on every read.
- Pinned by `tests/compat/scripts/protocol/reset-ignores-queued-work.bolt`
  and the `framing_tests.rs` unit tests.
