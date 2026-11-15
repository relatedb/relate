# fraud-qpp — in-house fraud QPP suite (LABS-646, ADR-0479)

Six re-authored fraud-ring queries exercising the SIMPLE path mode and
inline-WHERE quantified groups the other corpora don't. No external
data: f01/f02 run over the graph the `relate-datasets` fraud
generator builds (its planted cycle is their oracle), f06 over this
directory's designed fixture.

## Provenance and license

IN-HOUSE (Apache-2.0, this repository). The repos and training material
that inspired the suite are UNLICENSED or ©-Neo4j and were never copied
(verified 2026-10-31, LABS-646 pre-work `license-notes.md`): the only
facts carried over from the external spike run are performance/oracle
reference points (8 rings on the bipartite bounded form, the
1000→900→810→729 sample ring shape, 5 hand-enumerated date-ordered
cycles), recorded in `oracle.json`'s `_source` — facts, not expression.

## Entries

| id | what | status | oracle |
| --- | --- | --- | --- |
| `f01-monopartite-cycle` | directed 2-6 hop rings over the generator's Account/TRANSFER graph, anchored on the planted accounts | ok | planted-ring check: the generator's planted cycle must appear among the rings |
| `f02-simple-cycle` | the same anchor under the SIMPLE path mode | ok | planted-ring check (SIMPLE never removes a planted simple ring) |
| `f03-bipartite-bounded-ring` | bipartite (Account)-[:FROM]-(Transaction)-[:TO]-(Account) decay rings, bounded `{1,6}` | kept | the bipartite generator is follow-up work; oracle will be its planted ring ids |
| `f04-bipartite-unbounded` | f03 without the cap (`{1,}`) | kept | does not terminate today — no walk budget, no frontier kernel (LABS-644, LABS-660 item 3) |
| `f05-needle` | one planted long ring under noise | kept | the needle generator is follow-up work; oracle will be exactly the planted ring |
| `f06-sample-ring` | the designed 4-account sample ring, unit-scale | ok | rows check against `oracle.json`: exactly one ring, accounts 1-2-3-4-1 (`nodes(p)` lists the closed endpoint twice), amounts [1000, 900, 810, 729] |

`fixture-f06.cypher` loads the f06 world: the designed ring (10% decay
per hop, dates one day apart) plus five decoys — three that break the
decay or date rule, one that follows the rules but cannot close the
ring, and one isolated account.

f02 is anchored like f01 deliberately: the unanchored spelling
enumerates every simple cycle in the generated noise — output-exponential
at any interesting scale, a property of the data, not an engine defect —
while the anchored spelling tests the same SIMPLE restrictor against the
planted ring.

## File format

Same contract as the pathdb workload (see its README): one statement per
file, `// status:` marker line, no parameters (the queries anchor on
planted ids and literals). `oracle.json` carries per-entry checks —
`planted-ring` (runs on the generated graph), `rows` (runs on the
fixture, compared with the graph-benchmark-ldbc lane's no-coercion rules), `none` (kept).

## How to run

```bash
cargo run --release -p relate-query-latency -- corpus fraud-qpp
# the normal-lane test (fast, no data set download):
cargo test -p relate-query-latency --test corpus
```

Generator knobs (`--fraud-scale`, `--fraud-seed`) and the full flag
table: `docs/datasets.md` (the `query-latency corpus` section). The
fraud corpus test honours `RELATE_FULL_DATA=1` (the datasets
crate's own gate) to generate at full scale; the binary's
`--fraud-scale` overrides it.
