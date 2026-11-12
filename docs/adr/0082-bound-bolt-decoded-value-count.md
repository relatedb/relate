# 0082. Bound the number of decoded Bolt values per message

- **Status**: Proposed
- **Date**: 2026-09-23
- **Deciders**: CypherLite maintainers

## Context

Bolt messages are capped at 16,777,216 payload bytes and PackStream
containers at 128 levels (LABS-76). Those bounds do not constrain decoded
heap amplification: a list of one-byte scalars fits millions of elements in
one permitted message, but each decoded `Pack` occupies multiple machine
words and can then be cloned into query parameters. Even a successfully
bounded wire read can exhaust memory before the query engine runs.

## Decision

Reject incoming PackStream messages containing more than **262,144 decoded
values** in aggregate, including the root, container values, and map keys
and values. Count values during decoding, and reject declared list/map/struct
sizes that cannot fit the remaining value budget **before** allocating their
vectors. An over-budget message ends only its Bolt connection, as for other
malformed PackStream messages. The 16 MiB wire cap and 128-level depth cap
continue to apply independently. The count permits a 65,536-entry map32
(131,073 decoded values including its root).

The budget applies to incoming messages, not query results or outbound
encoding. A query-wide output/memory governor belongs to the later executor
stream (LABS-71/80/88); Bolt PULL governance belongs to the server stream
(LABS-79).

## Alternatives considered

- **Rely on the wire-byte cap alone.** Rejected: tiny wire values expand into
  much larger `Pack` structs and downstream query parameters.
- **Limit each list or map independently.** Rejected: nested or sibling
  containers could evade a per-container limit while still accumulating an
  excessive total.
- **Reject messages with more than 65,535 map entries.** Rejected: map32 is
  valid PackStream and already supported; a total value budget preserves
  useful larger maps without allowing millions of tiny elements.

## Consequences

- Some otherwise well-formed, densely populated messages within the 16 MiB
  wire cap close their connection at the value limit. Bolt documentation and
  regression tests must state and enforce the count.
- Input heap growth attributable to the number of decoded PackStream values
  is bounded in addition to payload bytes and nesting; outbound result size
  remains a separate governance concern.

## Later references

- ADR-0577: an over-budget message fails its request with a FAILURE and keeps
  the connection open instead of closing it.
