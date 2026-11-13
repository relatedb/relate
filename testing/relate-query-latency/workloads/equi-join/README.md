# equi-join — the cross-product join corpus (LABS-1531)

Three IN-HOUSE queries over the SAME pinned graph-benchmark-ldbc SF0.1 store
the `graph-benchmark-ldbc`, `pathdb`, `snb-bi` and `agg-stress` lanes run
(`workloads/graph-benchmark-ldbc/`, ADR-0243 — no new data), written as the
**measuring net for the planned hash-join operator** (LABS-1531). The
verdict LABS-1531 waits on: the engine has no join operator — two MATCH
patterns sharing no anchor multiply as a cross product inside the Expand
and the equality predicate filters after it (the `CartesianProduct`
notification, 03N90), so a query that joins two large lanes on a shared
property costs `|left| x |right|`, not `|left| + |right|`. No shipped
workload has the shape — the 30 graph-benchmark-ldbc queries, the snb-bi
analytics, the pathdb RPQs, ukrailway and agg-stress were EXPLAIN-scanned
(2026-11-13): zero cartesian-flagged entries. This workload exists so the
join lands as a lane-level timing delta instead of nothing: every entry is
two patterns sharing no anchor joined by a WHERE equality on a shared
property, and the three entries spread the product across the sizes that
distinguish enumeration cost from emission cost.

## Provenance and license

- Source: IN-HOUSE (2026-11-13), written for this workload. No external
  query was translated or copied — the shapes (selective-key control,
  fan-out key, cross-product headline) are synthetic join-stress designs
  over the graph-benchmark-ldbc vocabulary.
- Data: the pinned graph-benchmark-ldbc store at SF0.1 (CC BY 4.0,
  `workloads/graph-benchmark-ldbc/data.sha256`) — 327,588 nodes /
  1,477,965 relationships, of which this workload touches `Person`
  (1,528 nodes) and `Comment` (151,043 nodes; 7,621 with `length > 100`).

## Why these queries — the product and the output

The workload's one ingredient is the anchorless two-pattern MATCH whose
WHERE equality is the join. Cost today has two parts: enumerating the
product (`|left| x |right|` predicate evaluations) and emitting the
survivors (the pairs the equality admits). A hash join removes the first
and keeps the second, so the entries isolate each:

| Entry | Product pairs | Output pairs | Time | Rows |
| --- | --- | --- | --- | --- |
| e01-selective-equi-join | 2.33M (Person x Person) | 2,184 (birthday) | ~7 ms | 1 |
| e02-fanout-equi-join | 2.33M (Person x Person) | 723,576 (browserUsed) | ~1.1 s | 1 |
| e03-cross-product-equi-join | 11.6M (length>100 Comment x Person) | 3,537,568 | ~7.2 s | 1 |

e01 and e02 share the SAME product and differ only in output fan-out
(0.09% vs 31% survival): after a hash join e01 is near-free while e02
still emits its 723,576 pairs — the pair bounds what any join rewrite can
reach on this shape. e03 is the headline: a product-dominated entry over
the two large lanes.

## The measured ladder behind e03's `length > 100` bound

Timings 2026-11-13, release build, reused store copy, loaded shared
machine (load average ~20; the lane run on a quiet box is the
ratchet-facing number):

| Bound on the Comment side | Product | Output | Time |
| --- | --- | --- | --- |
| `length > 100` (7,621 comments) | 11.6M | 3.5M | 7.2 s |
| `length > 50` (50,339 comments) | 77M | 23.3M | 59.6 s |
| unbounded (151,043 comments) | 231M | 69.9M | 102 s |

`length > 50` (59.6 s) is past the lane's flat 30 s bound too, and
the unbounded form is past any budget, so the recorded entry takes
the `> 100` rung — the ladder is
recorded here so a future engine (the hash join itself) can re-measure
and raise the bound deliberately, the way agg-stress records its `*1..4`
rung.

## What a run reports

Every entry is `ok` at SF0.1, answer-checked against
`oracle-sf0.1.json` (a single count row each), constants inline — no
parameters, no seeds, the same lane shape as agg-stress. An SF1 run
fails loudly at its oracle load (no `oracle-sf1.json` exists).
