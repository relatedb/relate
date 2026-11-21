### Notes

- The never-sealed adjacency seal floor adapts: a chunk whose delta length
  held across a checkpoint interval now seals at 8 hops instead of the
  32-hop knee (ADR-0635), so a long-running store's steady mid-degree
  chunks fold instead of staying delta. The delta chunk stride grows 32 to
  40 B, and the Small memory shape's pins and the loadtest logical bytes
  move with it — the committed envelopes re-freeze through the ADR-0071 CI
  dispatch.
