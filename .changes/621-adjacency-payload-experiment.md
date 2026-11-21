### Added

- Experimental, default-off checkpoint payload for parquet snapshots that persists the graph's adjacency in its sealed form, so the next open installs it instead of re-folding the relationship rows. Opt in with `RELATE_ADJACENCY_PAYLOAD=1` (unset or `0` keeps it off; any other value, including empty, fails the checkpoint); `RELATE_ADJACENCY_PAYLOAD_MIN_RELS` (default 10,000,000 live relationships) gates it by store size. A payload that is missing, corrupted, torn, or disagrees with the rel rows on disk degrades to a warning plus the ordinary load path — never a refused store.
