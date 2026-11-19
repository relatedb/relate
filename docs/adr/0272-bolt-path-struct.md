---
type: adr
area: surface
date: 2026-09-29
deciders: CypherLite maintainers
name: 0272-bolt-path-struct
status: accepted
tags:
  - bolt
title: Bolt Path values encode as a PackStream structure
REFERENCES:
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
  - "[[adr/0082-bound-bolt-decoded-value-count]]"
  - "[[adr/0266-bolt-websocket-transport]]"
  - "[[adr/0325-bolt-temporal-results-as-packstream-structs]]"
  - "[[adr/0326-bolt-point-results-as-packstream-structs]]"
---

# 0272. Bolt Path values encode as a PackStream structure

- **Renumbered**: from 0143 (shared-number cleanup)

## Context

CypherLite's Bolt transport ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)) maps engine values to PackStream via
`from_mdb_at` in `surfaces/cypherlite-bolt/src/packstream.rs`. Nodes and
relationships already encode as proper PackStream structures — a Node is
`Struct 0x4E` (id, labels, properties) and a Relationship is `Struct 0x52`
(id, start, end, type, properties) — so drivers render them as graph elements.

`Value::Path` is the exception: at
`surfaces/cypherlite-bolt/src/packstream.rs:129` it is encoded as
`Pack::String(v.to_cypher_literal())`. A driver therefore receives a Cypher
literal string where it expects a Path structure; the `Path` type cannot be
reconstructed, patterns cannot be rendered as a graph, and the value round
trips only as opaque text. This is LABS-147.

The server negotiates Bolt **4.4** only: `protocol.rs:168` writes
`[0x00, 0x00, 0x04, 0x04]`. The Bolt 4.x Path wire form is the structure
described below; Bolt 5.x's element-id struct forms are not reachable on the
current negotiation. The engine builds `Path { nodes, rels }` in traversal
order at every construction site, and hop `i` is forward exactly when
`rels[i].start == nodes[i].id`.

This fix must respect the decode-side bounds of [ADR-0082](./0082-bound-bolt-decoded-value-count.md) and must not disturb
the Temporal/Point/Vector string encodings that share the same match arm
family. The decision was agreed by the maintainer in LABS-147 before this ADR
was written.

## Decision

`Value::Path(path)` encodes as a PackStream **Structure with tag `0x50`** and
exactly three fields:

1. **nodes** — a List of Node structures (`0x4E`), in traversal order, each
   encoded exactly as the existing `Value::Node` arm (id, labels, properties);
2. **relationships** — a List of **UnboundRelationship** structures (tag
   `0x72`), in traversal order, each with fields `[id, rel_type, properties]`
   (`Int`, `String`, property `Map`) — the bound `start`/`end` endpoints are
   deliberately dropped because the sequence below re-derives them;
3. **sequence** — a List of Int, giving the traversal path.

The **sequence** contains **two entries per hop**:

- the **1-based index of the relationship** used by the hop, made **negative
  when the hop traverses that relationship against its start-to-end
  direction**. A hop is forward iff `rels[i].start == nodes[i].id`, in which
  case the entry is `i + 1`; otherwise it is `-(i + 1)`.
- the **0-based index of the node arrived at** by the hop, which in traversal
  order is `i + 1`.

The whole structure is produced by returning `Pack::Struct { tag: 0x50, .. }`
from the `Value::Path` arm, **recursively** via the existing `from_mdb_at`
call for each contained node and each unbound relationship. Because
`from_mdb_at` recurses through the existing `child_depth` depth accounting,
the same encoding applies at **any nesting depth** — a Path returned inside a
List, Map, or nested collection is encoded identically. The Temporal, Point,
and Vector arms remain **string encodings**, unchanged.

## Alternatives considered

- **Keep stringifying Path to a Cypher literal.** Rejected: drivers cannot
  render a graph from a string; the client loses the `Path` type, cannot
  reconstruct nodes/relationships, and cannot display the pattern. This is
  precisely the LABS-147 defect.
- **Negotiate Bolt 5.x for the element-id struct forms of Node,
  Relationship, and UnboundRelationship.** Rejected: it is far heavier than
  the bug fix — it would change the handshake contract, the version the
  server offers (currently 4.4 only), and the wire form of every existing
  node and relationship — while the Bolt 4.x `0x50` Path structure already
  solves the reported problem on the protocol the server actually speaks.

## Consequences

- A `Path` returned over Bolt is a real PackStream structure (`0x50`) that
  conformant drivers decode into a graph pattern rather than a string; the
  Node (`0x4E`) and UnboundRelationship (`0x72`) members reuse the existing
  Node encoding and a new three-field relationship form.
- The PackStream encoder must gain the `0x50` structure and the `0x72`
  unbound-relationship struct; no new protocol version or handshake change
 is needed, preserving [ADR-0057](./0057-embedded-c-abi-and-bolt.md)'s "Bolt is a transport" boundary.
- Encoding stays bounded by the existing `child_depth` recursion and
 [ADR-0082](./0082-bound-bolt-decoded-value-count.md)'s decode limits; no new nesting budget is introduced.
- Temporal, Point, and Vector stringification is unchanged, so existing
  client expectations for those types are untouched.
- Tests must pin the `0x50` tag and field order, the `0x72` field order,
  forward and reverse hop signs, the two-entries-per-hop sequence, and that a
  Path nested in a collection encodes identically.
- Builds on [ADR-0057](./0057-embedded-c-abi-and-bolt.md) (Bolt transport boundary) and [ADR-0082](./0082-bound-bolt-decoded-value-count.md) (decode bounds),
 and is compatible with [ADR-0266](./0266-bolt-websocket-transport.md) (Bolt over WebSocket / browser clients),
  which carries the same values over an alternate transport.

## Later references

- [ADR-0325](./0325-bolt-temporal-results-as-packstream-structs.md): supersedes "The Temporal ... arms remain string encodings".
  Temporal results encode as the Bolt 4.4 PackStream temporal structures
  (`0x44`, `0x54`, `0x74`, `0x46`, `0x66`, `0x64`, `0x45`).
- [ADR-0326](./0326-bolt-point-results-as-packstream-structs.md): supersedes "The ... Point ... arms remain string encodings".
  Point results encode as Point2D `0x58` / Point3D `0x59` with Float
  coordinates. Vector results stay strings; the Path decision here is
  unchanged.
