# 0180. Checkpoint shards stream, split on compact bytes, and keep clean rows

- **Status**: Accepted
- **Date**: 2026-10-06
- **Deciders**: CypherLite maintainers

## Context

ADR-0022 publishes a checkpoint as id-window Parquet shards aimed at 256 MiB.
ADR-0086 caps a decoded shard at 2 GiB and a decoded row at 384 MiB. The
writer enforced the row-count side of that with a 65,536-row cut and a
64-row prefix sample, then built one `RecordBatch` for the surviving window.

One dirty id rewrote every row in its window. `node_at` / `rel_at` clone
every property into a map, and the batch held those maps, their JSON, and
the Arrow columns together. A small property update on a neighbor of a large
string re-encoded the string. The 64-row sample could approve that window
when the large rows sat past the prefix.

ADR-0284 still applies: page statistics stay off. ADR-0086's envelope still
applies. This decision changes how a window is chosen and written.

## Decision

1. **Stream row groups.** A shard file is still one id window. The writer
   feeds Arrow at most 64 rows at a time, and flushes that row group before
   the next. A group also flushes before its decoded bytes would pass the
   ADR-0086 decoded envelope. Snappy and `EnabledStatistics::None` stay.
   Node and relationship shards use the same writer.

2. **Split on compact bytes.** Before encoding, sum each live row's decoded
   size from compact property storage: the fixed row header, label or type
   bytes, and the tagged JSON of the properties. The walk borrows values. It
   does not build a property map and it does not allocate the JSON string.
   Cut the id window when the next row would pass the shard target or the
   decoded envelope. The 65,536-row cut and the 64-row sample are gone. A
   single row that cannot fit the row envelope, or a single row that cannot
   fit the decoded envelope, is still an unsplittable shard.

3. **Copy clean rows.** A part with no dirty id stays as published. A part
   with a dirty id and a previous Parquet file is walked in id order. A run
   of clean ids is copied from that file, one reader batch at a time, into
   its own non-overlapping `[id_lo, id_hi)` part. That copy does not load
   the live node or rebuild JSON. A run of dirty ids is encoded from the
   live graph and split by the compact byte count. The next checkpoint then
   sees no dirty id in the clean part and keeps the file.

The first checkpoint has no previous file, so every row is encoded, streamed,
and split by the compact byte count.

## Alternatives considered

- **Keep the 64-row sample.** Rejected: a prefix of small rows approves a
  window of large ones, and the sample encodes rows the byte count already
  measured.
- **Halve a dirty window and re-encode every half.** Rejected: a clean row
  in the dirty half is still materialized. The copy separates those rows
  without reading them from the live graph.
- **Narrow id windows only.** Rejected: neighbors that share a window are
  still rewritten together. Byte splitting isolates a fat row on the next
  write; the copy avoids rewriting it on this write.

## Consequences

A checkpoint's live peak for one shard is one row group, plus the previous
file's bytes while a clean run is copied. A property update encodes the
dirty rows and leaves the other rows in the previous file's values. Shards
already published stay readable. A row that exceeds the ADR-0086 row
envelope still fails the checkpoint with the same decoded-row error.

## Later references

ADR-0183 supersedes Decision 3's one part per clean run: a rewrite unit
copies clean runs into its byte-budgeted files together with the encoded
dirty rows, and absorbs undersized neighboring parts up to the target.
Decisions 1 and 2 stand.

ADR-0563 raises Decision 1's 64-row row-group cap to 1024 rows (the wide
encoding's leaf count made the small group the dominant write tax); the
streaming, byte-split and copy shape of all three decisions stands.
