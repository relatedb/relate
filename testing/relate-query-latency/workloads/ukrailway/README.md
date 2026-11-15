# ukrailway — UK railway graph corpus (ADR-0479's deferred fourth lane)

The two verified UK railway graph queries over the workload's OWN graph:
the dogofbrian/ukrailway data set imported into `:memory:` by
`import.cypher` — not the graph-benchmark-ldbc store, not a generated graph. r02 counts
connected (origin, destination) station pairs over calling-point chains
with a two-leg quantified pattern; r03 finds the ANY SHORTEST
Denmark Hill → Gatwick Airport route over the station LINK graph.

## Provenance and license

- Data: [dogofbrian/ukrailway](https://github.com/dogofbrian/ukrailway)
  at the pinned commit (`PINNED_COMMIT` in `src/fetch.rs`, upstream
  commit date 2024-07-04), **Apache-2.0** (repo-root `LICENSE`,
  verified in a local clone).
  Eight headered CSVs (18 MB) under `data/` — fetched on demand from
  `raw.githubusercontent.com` at the pinned commit, verified against
  `data.sha256` (per-file sha256, re-verified every run), **never
  committed**. Cached under the data cache root's `ukrailway/`
  (`--data` / `RELATE_QUERY_LATENCY_DATA`, the same cache the graph-benchmark-ldbc
  lanes use).
- Queries: **IN-HOUSE**. No query text exists upstream for r02/r03 — the
  repo's own flagship query (r01, the two-leg timetable search) is out of
  scope here (minutes-scale at 4 m 12 s in the 2026-10-31 spike, tracked
  with the walk-budget class in LABS-642 item 4 / LABS-644). r02/r03 were
  drafted in-house, and both the query texts and their answers were
  verified by execution against the pinned data set on **2026-11-02**:
  - **r02-route-pairs** — `routePairs = 1248`, `changeoverStations = 19`
    (the 2026-10-31 spike had verified the same values; its query text
    was not saved, so this file re-derives and re-verifies them). The
    earlier draft counted DISTINCT changeover *calling points* under the
    key `changeoverCallingPoints`; the verified query counts DISTINCT
    changeover *stations*, and the expected key is `changeoverStations`.
  - **r03-shortest** — `hops = 15`, the 16-station list in `oracle.json`
    (Denmark Hill → Brixton → … → Gatwick Airport: geographically the
    Brighton main line approach via Crystal Palace and East Croydon).
- Oracle: `oracle.json`, one rows check per entry (the shared
  `expected-sf<SF>.json` shape, no coercion — the same rules the graph-benchmark-ldbc and
  fraud lanes compare with). The oracle is the 2026-11-02 verified
  answer, recorded once — an in-house verification, never claimed as
  upstream output.

## Lane shape

`UkrailwayLane` (src/corpus/ukrailway.rs) builds one `:memory:` store
whose LOAD CSV policy is confined to the fetched data directory
(ADR-0081), then runs `import.cypher`'s statements in order (the
canonical splitter). Every LOAD CSV source in the import is
root-relative, resolved by that policy under the fetched directory — no
path is spelled in the script itself, and no read can leave the
directory. The import creates the Station/CallingPoint/Stop nodes, the
LINK/NEXT/CALLS_AT/HAS edges and the id/name lookup indexes the queries
anchor on (~2649 stations, 2902 LINK, 293 801 NEXT, 52 582 CALLS_AT;
import ≈ 30 s, release).

No parameters (the queries carry their constants inline — the snb-bi
lane's params machinery is not forced onto a workload with nothing
parameterized), no seed policy (nothing anchors on a node id), no scale
factor (the data set has one size — the report records neither).

## Timeout policy

r02 is minutes-scale: **213.8 s** first execution in the lane (release,
2026-11-02 landing run; a 4 m 33 s earlier probe figure included the
probe's process start and store open), Neo4j's blog states ~100 ms for
the equivalent shape. The lane therefore defaults to its OWN per-entry
timeout — the lane's own `Lane::default_timeout` resolves 600 s for
ukrailway, ~2.8x the measured r02 — never the scale-factor-scaled
default: the workload has no scale factor to scale it with,
and the 10 s graph-benchmark-ldbc default would fail the ratchet on day one. A timeout is
still a ratchet failure for an `ok` entry: the bound is generous, not
absent. r03 is milliseconds-scale in-lane (~4 ms first, ~3 ms median —
the ANY SHORTEST walk anchors on the name index); the landing run's
peak RSS was 1.80 GB.

Regeneration rule: a new pin (upstream commit) touches exactly one hash
spelling — `PINNED_COMMIT` in `src/fetch.rs` (the URL base formats from
it) — plus `data.sha256`, whose per-file checksums (and header line
naming the new pin) change with the data. The import script's header
and this README quote the pin by reference, never by value, so neither
is part of a pin bump; the query headers and `oracle.json` name the pin
they were verified against, which a pin bump re-verifies and re-records
anyway — changed answers re-verify by execution and re-record
`oracle.json` — never edit the oracle to match an unverified engine
answer.

## How to run

```bash
cargo run --release -p relate-query-latency -- corpus ukrailway
# the gated test (fetches the data set on first use, then imports it):
RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p relate-query-latency --test corpus ukrailway
```
