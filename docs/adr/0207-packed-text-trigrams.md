---
type: adr
area: engine
date: 2026-10-11
deciders: CypherLite maintainers
name: 0207-packed-text-trigrams
status: accepted
tags:
  - index
title: Packed width-3 TEXT postings
REFERENCES:
  - "[[adr/0018-concurrent-snapshot-reads]]"
  - "[[adr/0030-string-prefix-trigram-indexes]]"
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0063-lazy-string-index-postings]]"
---

# 0207. Packed width-3 TEXT postings

- **Amends**: [ADR-0030](./0030-string-prefix-trigram-indexes.md), [ADR-0063](./0063-lazy-string-index-postings.md) (trigram posting shape and the text term of the [ADR-0042](./0042-graph-memory-limit.md) formula)

## Context

`CREATE TEXT INDEX` posted every overlapping 1-gram, 2-gram, and 3-gram of each indexed string. Each gram was an owned `String` key in `HashMap<String, HashSet<NodeId>>`, and every node kept a second `HashSet<String>` of those grams so an update could find them after the property changed. A string of `C` characters therefore stored about `3C` windows, twice, and the 1-grams pointed at almost every node.

`CONTAINS` only needs a superset of the true matches. The seek rechecks `contains` at the snapshot ([ADR-0018](./0018-concurrent-snapshot-reads.md)).

## Decision

Store exactly the raw width-3 windows. A string of `C` characters posts `C - 2` trigrams when `C >= 3`, and nothing otherwise.

- The key is a `u64`: three Unicode scalars, 21 bits each. The map holds no per-key heap buffer.
- Posting lists are sorted, deduplicated `Vec<NodeId>`. A bulk build appends, then sorts once. A later single-node edit binary-searches. A seek intersects from the shortest list and probes the others; it does not clone a posting list.
- Each node keeps the source `Arc<str>`, shared with the property value. Removal recomputes the trigrams from that arc. There is no per-node copy of the gram strings.
- A needle shorter than three characters, including the empty needle, returns no candidates from the index. The caller scans, and the residual `contains` check is unchanged. Intersecting space-padded keys (the `pg_trgm` boundary trick) misses a two-character match in the middle of a value, so those keys are not stored.
- The logical memory charge stays a formula of the live node: one `OVERHEAD_POSTING` per width-3 window. The source string is the property already counted on the record. Label length, key length, and a map entry are not charged on every window.

Case sensitivity is unchanged. Range and point indexes are unchanged.

## Consequences

- Text-index memory scales with the number of width-3 windows, not with three widths of owned strings and a reverse string set.
- `CONTAINS` of one or two characters scans the label even when a TEXT index exists. Those needles were the least selective postings.
- `used` for a text-indexed string drops to `8 * max(char_len - 2, 0)` bytes of index charge on top of the property itself. The incremental counter and the live-walk oracle still share `node_account`.
