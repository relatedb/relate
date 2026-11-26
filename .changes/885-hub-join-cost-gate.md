### Notes

- Executor (REL8-2253, ADR-0701 proposed): the two-pattern comma hub join now
  engages only when its one enumeration of the second pattern is estimated no
  dearer than the nested loop's per-row expansions, so a selective first
  pattern no longer pays a full scan of the second (graph-benchmark Q6 comma
  form 147 -> ~22 ms). No query answers change.
