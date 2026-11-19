---
type: adr
area: surface
claim: 4d1b4621cb4d85399bc0e0b76153c18e9baf52cf
name: 0316-bolt-client-noop-chunks
status: proposed
tags:
  - bolt
title: Bolt accepts client NOOP chunks between messages
REFERENCES:
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0312-bolt-hello-recv-timeout-hint-and-noop-keepalive]]"
---

# ADR-0316: Bolt accepts client NOOP chunks between messages

## Context

Since Bolt 4.1 either side may send a NOOP: an empty chunk (`00 00`)
where a message would start. It keeps an idle connection alive through
middleboxes and is not a message. Any number of NOOPs may precede or
separate messages.

`read_message` in cypherlite-bolt reads chunks until an empty one ends the
message. A NOOP therefore reads as a message with an empty payload, and
`handle_conn` closes the connection on it. The LABS-323 probe (finding
F-29) sent a NOOP between RUN and PULL: latest Neo4j answered the PULL,
cypherlite-bolt closed.

## Decision

1. An empty chunk received where a message would start is a NOOP. The
   server skips it and keeps reading. Any number of NOOPs may precede or
   separate messages.
2. A NOOP counts as connection activity for the [ADR-0110](./0110-bolt-connection-query-governance.md) idle timeout: it
   re-arms the wait for the next message. It is not a message, so it gets
   no reply and does not change session state.
3. The NOOP rule lives in the one chunk-framing helper that both the
   server's message reader and the public `protocol::read_message` use, so
 test clients skip the server's [ADR-0312](./0312-bolt-hello-recv-timeout-hint-and-noop-keepalive.md) keep-alive NOOPs by the same
   rule.

## Alternatives considered

- **Keep closing on an empty payload.** Rejected: NOOPs are legal Bolt
  4.1+, and drivers or proxies that send them lose their connection.
- **Skip NOOPs without re-arming the idle timeout.** Rejected: a NOOP is
  the client saying it is alive, which is exactly what the idle timeout
  measures.

## Consequences

- Clients and proxies may send NOOPs between messages.
- A NOOP-only stream keeps a connection open past the idle timeout, the
 same as Neo4j; the connection cap ([ADR-0110](./0110-bolt-connection-query-governance.md)) still bounds connections.
- Pinned by `tests/compat/scripts/protocol/client-noop-chunk.bolt` and the
  `framing_tests.rs` unit tests.
