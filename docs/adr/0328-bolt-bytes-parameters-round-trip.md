---
type: adr
area: surface
claim: 8081e1a2029d685bae052b557c6edc74323e41a3
name: 0328-bolt-bytes-parameters-round-trip
status: proposed
tags:
  - bolt
  - values
title: Bolt bytes parameters round-trip through a transient engine bytes value
REFERENCES:
  - "[[adr/0177-bolt-parameter-conversion]]"
  - "[[adr/0219-python-parameter-conversion]]"
  - "[[adr/0327-engine-statement-scoped-bytes-value]]"
SUPERSEDES:
  - "[[adr/0177-bolt-parameter-conversion]]"
---

# ADR-0328: Bolt bytes parameters round-trip through a transient engine bytes value

## Context

[ADR-0177](./0177-bolt-parameter-conversion.md) rejects a PackStream Bytes parameter (markers `CC`/`CD`/`CE`) at
any depth with `unsupported parameter value for {path}: Bytes`, because
the engine had no value to hold it. Neo4j 2026.09.0 accepts it and returns
it unchanged: `RUN 'RETURN $b AS b' {b: bytes 00 01 FF}` answers
`RECORD [bytes 00 01 ff]`, `CC 03 00 01 FF` on the wire (F-64). neo4rs
expects `BoltType::Bytes` back (D-25). CypherLite fails the RUN instead.

[ADR-0327](./0327-engine-statement-scoped-bytes-value.md) adds the engine value `Value::Bytes`, statement-scoped by the
user's 2026-10-16 decision.

## Decision

1. A PackStream Bytes parameter, at any depth (top level, list element,
 map value), decodes to `Value::Bytes` with the same bytes. [ADR-0177](./0177-bolt-parameter-conversion.md)'s
 Bytes rejection and its message row are removed. The other [ADR-0177](./0177-bolt-parameter-conversion.md)
   rules (struct tags, map keys, the params envelope) are unchanged.
2. A `Value::Bytes` result, at any depth, encodes as PackStream Bytes with
   the shortest length marker: `CC` up to 255 bytes, `CD` up to 65535,
   `CE` beyond.
3. The value is statement-scoped ([ADR-0327](./0327-engine-statement-scoped-bytes-value.md)): storing it as a property
   fails the statement with the typed `TypeError`, which Bolt answers as
   `Neo.ClientError.Statement.TypeError`. The spec cites the Linear
   follow-up "Engine: persist bytes (byte-array) values as node and
   relationship properties (WAL, snapshot and Parquet/JSON checkpoint
   codecs)".

## Alternatives considered

- **Keep rejecting Bytes ([ADR-0177](./0177-bolt-parameter-conversion.md)).** Rejected: drivers that send binary
  parameters fail where Neo4j succeeds, which is the F-64/D-25 divergence.
- **Decode Bytes to a `LIST<INTEGER>` like [ADR-0219](./0219-python-parameter-conversion.md).** No engine change.
  Rejected: the result would come back as a list, so the parameter would
  not round trip as bytes.
- **Accept bytes for storage as well.** Rejected by the user's decision;
  the persistence follow-up owns it.

## Consequences

- `RETURN $b` returns the driver's bytes unchanged (Python `bytes`, neo4rs
  `Vec<u8>`), also inside lists and maps.
- `docs/api/bolt/spec.md` drops the Bytes rejection row, lists Bytes in
  both the parameter and result tables, and documents the property error.
- [ADR-0177](./0177-bolt-parameter-conversion.md) gets a Later references note.
- `tests/compat/scripts/values/bytes-param-roundtrip.bolt` and unit tests
  pin the round trip; `docs/api/bolt/examples/temporal_params.py` replaces
  its rejection block with a round trip.
