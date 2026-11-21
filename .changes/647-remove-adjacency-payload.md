### Notes

- The adjacency-payload checkpoint experiment (#621, REL8-1256) was
  removed from main before any release cut on its measured verdict: the
  parquet-row payload never paid at any measured store size (10M
  mid-degree: 17% open win against a recurring per-checkpoint penalty),
  and the cost sits in the parquet column layer in both directions. Its
  env vars (`RELATE_ADJACENCY_PAYLOAD`,
  `RELATE_ADJACENCY_PAYLOAD_MIN_RELS`) never shipped; the measurement
  record lives on REL8-1256 and the code is recoverable from the revert
  commit and PR #621.
