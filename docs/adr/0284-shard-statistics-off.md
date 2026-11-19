---
type: adr
area: engine
date: 2026-10-05
deciders: CypherLite maintainers
name: 0284-shard-statistics-off
status: accepted
tags:
  - checkpoint
  - parquet
title: Shard writer stores no page statistics
REFERENCES:
  - "[[adr/0086-bounded-persistence-input]]"
---

# 0284. Shard writer stores no page statistics

- **Renumbered**: from 0164 (shared-number cleanup)

## Context

[ADR-0086](./0086-bounded-persistence-input.md) §3 parses each Parquet page header before decompression and limits
that parse to 64 KiB. The shard writer uses Arrow's default statistics,
which is page level with no truncation. For a string column, the page header
then contains the full minimum and maximum values on that page.

Node and relationship properties are one string column. A property value can
be far larger than 64 KiB. The header parse steps over that whole value, runs
past the 64 KiB window, and rejects the shard with `page header length`. The
loader reads every row of a shard into memory and never consults the minimum
or maximum.

## Decision

The shard writer disables Parquet statistics (`EnabledStatistics::None`).
Property values stay in the page payload. Both node and relationship shards
go through this writer.

A later ADR may turn statistics back on as a prefix of the minimum and
maximum value, short enough to fit in the 64 KiB header, if pagination needs
to skip pages on disk.

## Alternatives considered

- **Store the full minimum and maximum.** Rejected: a property string can
  exceed the 64 KiB header window, and the loader does not read the header
  value.
- **Store only the size of the minimum and maximum.** Rejected: the Parquet
  field is the value, and a size cannot skip a page.
- **Truncate to a prefix now.** Rejected: no reader skips pages. A prefix is
  the shape to restore if one does, not the current writer setting.

## Consequences

A checkpoint of a graph whose properties are larger than 64 KiB passes the
[ADR-0086](./0086-bounded-persistence-input.md) page-header preflight. Shards already published are unchanged. The
preflight still runs before a shard is renamed into place, so a header that
fails it is not published.
