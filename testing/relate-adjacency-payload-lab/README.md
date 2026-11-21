# Adjacency-payload A/B lab

Measures the opt-in adjacency checkpoint payload (ADR-0551) against the
rel-row re-fold it replaces, per store size, and prints the threshold
verdict. Companion to `testing/relate-codec-lab` (same measurement-harness
convention: a `testing/` binary crate, never a shipped surface, with its
numbers committed in `RESULTS.md`).

The payload is a checkpoint-time cache tier: a checkpoint at or above
`RELATE_ADJACENCY_PAYLOAD_MIN_RELS` (default 10,000,000 live relationships)
writes one `adjacency/payload-<hex>.parquet` object when
`RELATE_ADJACENCY_PAYLOAD=1`; an open whose manifest lists it installs the
chunks instead of re-folding adjacency from the relationship rows.

## What the harness does

Three stores are built once, in memory, from `relate-datasets` shapes and
their closed-form `census` (the scale is tuned so the census lands nearest
the target relationship count):

| store | shape | target rels | census nodes / rels |
| --- | --- | --- | --- |
| `dense-ring-2m` | `dense-ring` | 2,000,000 | 100,000 / 2,000,000 |
| `mid-degree-2m` | `mid-degree` | 2,000,000 | 21,984 / 2,000,544 |
| `mid-degree-10m` | `mid-degree` | 10,000,000 | 109,904 / 10,001,264 |

Each store is checkpointed **once per arm** — payload on, payload off — with
`RELATE_ADJACENCY_PAYLOAD_MIN_RELS=0` so both arms publish at every size (the
default 10M threshold would skip the ~2M stores). The arm toggles only
`RELATE_ADJACENCY_PAYLOAD`.

Per store and per arm, the harness records, each stamped with the host's
1/5/15-minute load averages:

- **publish** — the wall of the checkpoint that publishes the store (and, in
  the on arm, the payload); the recurring cost an enabled payload charges
  every checkpoint.
- **open** — the snapshot load wall plus its **adjacency-tail share**: the
  payload install (on) or the rel-row re-fold (off), measured by
  `relate_storage::load_timed`.
- **read** — a fixed query set (out-degree, in-degree and a two-hop
  expansion around three nodes, plus the graph's total relationship count).
- **write** — a fixed statement batch (1,000 fresh nodes and 100 links).
- **close** — the `Database` drop checkpoint after the write batch.

The runs are paired and interleaved: `rounds` × (payload on, payload off).
The default is two rounds, i.e. arm A, arm B, arm A, arm B.

## Reproduce

Release build, quiet machine (the quiet-box protocol: stamp and report the
load; fresh build trees under `/private/tmp`, not Spotlight-indexed paths):

```sh
cargo build --release -p relate-adjacency-payload-lab
./target/release/relate-adjacency-payload-lab \
    --rounds 2 --out adjacency-payload-ab.json
```

The results JSON goes to stdout and to `--out`; progress goes to stderr.
Flags:

- `--rounds N` — interleaved rounds per arm (default 2).
- `--out PATH` — also write the results JSON there.
- `--only NAME` — run one store (`dense-ring-2m`, `mid-degree-2m` or
  `mid-degree-10m`).
- `--workdir DIR` — scratch root (default `/private/tmp/relate-adjacency-payload-lab-<pid>`; removed afterwards).
- `--smoke` — the self-check: the same three stores at 1/100 the target
  relationships (20k / 20k / 100k).

The harness sets `RELATE_ADJACENCY_PAYLOAD=1` (on) or leaves it unset (off)
and always sets `RELATE_ADJACENCY_PAYLOAD_MIN_RELS=0`; it restores nothing,
so run it in a fresh shell. The stores are built in a scratch directory and
removed at the end — budget the disk for the ~10M store plus its per-run
copies.

## Operational notes

- The store open is the snapshot load (`relate_storage::load_timed`), not the
  full `Database::open`: it is the phase the payload exists to cut, and it
  reports the adjacency-tail share in the same call. The read/write/close
  phases run on a `Database` opened over the same directory.
- A `:memory:` master graph is checkpointed to the two arm directories and
  dropped before the runs, so the measured runs never hold two large graphs.
- Each run opens a pristine copy of its arm's published store; the write
  batch dirties it and the drop publishes a fresh checkpoint.
- Wall times are load-sensitive. `RESULTS.md` records the load stamps of its
  run; re-measure on an idle box before trusting sub-10% differences.
