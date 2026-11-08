# Kernel coverage inventory: query shapes against compiled kernels

This note inventories the `MATCH` shapes the engine can encounter, records
which compiled operator serves each one at HEAD, and proposes an ordered
candidate list toward the goal of ~75% of corpus queries served by compiled
kernels rather than the per-row walker.

It is **not an ADR**: it records no decision. Each candidate that is accepted
gets its own ADR before implementation. The authoritative records for what is
contracted today are ADR-0468 (batch substrate), ADR-0473 (contracted
traversal kernels), ADR-0478 (frontier-NFA), ADR-0519 (diagnostic engine
switch), ADR-0521/0523/0524 (frontier tables, walk count, plain-cap rider).

Two standing corrections to earlier recollections, both verified here:

- There is **no "15 core patterns" inventory** in the ADRs. ADR-0473 records
  exactly two traversal families (chain, hub-star) plus the scan family; the
  per-shape bodies that existed were three research kernels, quarantined by
  ADR-0476 (see §2.6).
- The sink-projection gate is **not** "every single-clause statement". It
  excludes every statement whose following clause is a *sinkable* projection
  (non-aggregate, non-star, not an eager page). Aggregate returns and plain
  `LIMIT` pages pass; `DISTINCT` and `ORDER BY` returns never do. §1.1 states
  the exact rule.

Everything below is a **code-path classification**, not an execution
measurement. No query was run and no build was made while writing this note;
each claim names the branch that decides it. Where a cost gate or a corpus
marker could change the answer, the row says "unverified".

## 1. The ladder at HEAD

One call site: `relate/relate-executor/src/exec/dispatch.rs:360` (the SEARCH
arm bypasses it, `dispatch.rs:295-314`). The fold tier below runs before any
`MATCH` rung (`dispatch.rs:316-328` → `try_match_folds`, `dispatch.rs:697`).

| Tier | Operator | Engages at | Serves |
| --- | --- | --- | --- |
| fold A | boolean-count exists fold | `exec/exists_probe.rs:333`, gate `boolean_count_span` `exists_probe.rs:238`, threshold `counted_existence` `:299` | one or more plain `MATCH` clauses + ungrouped `count(DISTINCT e) > 0 \| >= 1` |
| fold B | walk-count | `exec/kernels/frontier_count.rs:54` | bounded `REPEATABLE ELEMENTS` under a plain `count`, unfiltered target, walk pool |
| fold C | REPEATABLE decline | `dispatch.rs:725-736` | multi-hop `REPEATABLE ELEMENTS` falls to the ladder |
| fold D | path agg fold | `exec/agg_fold/path.rs:635`, return gate `agg_fold/mod.rs:92`, shape gate `agg_fold/plan.rs:349` | one streamable pattern of 1–2 hops, or two 1-hop patterns sharing one node; ≥1 aggregate |
| fold E | one-hop agg fold | `exec/agg_fold/one_hop.rs:170` | exactly one pattern, exactly one rel, key items within the hop |
| rung 1 | frontier-NFA | `exec/match_clause.rs:81` → `kernels/frontier_run.rs:67` | one pattern with exactly one quantified element; pair-shaped answer (`DISTINCT`/endpoint fold, `LIMIT`, `EXISTS`) |
| rung 2 | scan-filter chain | `match_clause.rs:100-104` → `exec/batch.rs:204` | single node, labels, property predicates |
| rung 3 | chain + hub-star | `match_clause.rs:108-121` → `exec/kernels/mod.rs:195` | fixed 2–4-hop chain; 2-pattern hub-star |
| rung 4 | per-row walker | `match_clause.rs:128-141` | everything declined |

The exists side is a second ladder (`exists_probe.rs:65`): the contracted
kernels' exists arm (`kernels/mod.rs:244`) runs first, then the depth-first
`seek` (`exists_probe.rs:120`). The LABS-348 hub join is attempted inside
the walker at `match_clause.rs:304-316`, gated by `hub_join.rs:65` plus the
128-row side floor (`hub_join.rs:33`, `:161`).

Two structural facts shape the whole inventory:

- **Rung 1 is not behind the shared cap/projection gate** (`match_clause.rs:81-92`).
  It reads the statement suffix itself (`frontier_match.rs:432`, `decode_consumer`)
  and applies the sink projection with `reduce_row` (`frontier_run.rs:110-116`).
- **Rungs 2 and 3 share one admission gate** (`match_clause.rs:99`):
  `chain_eligible = (cap.is_none() || plain_want.is_some()) && projection.is_none()`.

### 1.1 The shared gate, precisely

`projection` is the clause *following* the `MATCH`, decoded at
`dispatch.rs:354` by `sinkable_projection` (`exec/project.rs:162-211`). That
function returns `Some` for a following `RETURN`/`WITH` unless one of these
holds (`project.rs:172-209`): a `GROUP BY`, a `*` item or any aggregate item,
an internal `__`-namespace alias collision, an aggregate `ORDER BY` key or
aggregate `WITH` filter, a `WITH` filter naming a name the projection does not
produce, or *eager page* (`!distinct && order.is_empty() && where.is_none()
&& (skip || limit)`).

Consequences, and the correction the header states:

- `MATCH … RETURN n.prop` → sink projection `Some` → the clause goes to the
  walker. This is the dominant read shape and the entire pokec-bench family
  named in LABS-1119.
- `MATCH … RETURN count(*)/count(x)` → aggregate item → projection `None` →
  the gate is open; the fold tier usually takes it anyway.
- `MATCH … RETURN … LIMIT n` (no `WHERE`, no `ORDER BY`, no `DISTINCT`) →
  eager page → projection `None` AND a plain cap (`plain_want`,
  `match_cap.rs:217`) → gate open, and the cap rides the kernel as its halt
  (ADR-0524).
- `MATCH … RETURN DISTINCT …` (with or without `LIMIT`) → projection `Some`
  (the eager-page test requires `!distinct`), and a distinct cap answers
  `plain_want == None` (`match_cap.rs:220`) — both halves close.

Pinned by the gate suite: `MATCH … RETURN a.k AS ak` is a 0-engagement
decline ("A sink projection runs in the match sink",
`exec/kernels_tests/gate.rs:37-38`), every engaging case in that file carries
`LIMIT n`, and `RETURN DISTINCT … LIMIT 2` is a decline (`gate.rs:39-40`).

The gate is a correctness interlock, not a heuristic: `dispatch.rs:373` sets
`RowSource::SinkProjected` whenever a projection exists, so a kernel that
took the clause without applying the projection would leave the following
clause reading columns that were never evaluated.

## 2. Shape inventory

25 structural classes. "Served by" names the compiled operator that owns the
class at HEAD; "walker" is the per-row loop. A class marked *conditional* is
owned only when the §1.1 admission gate and the §2.5 cost gates both pass.

### 2.1 Fixed-hop shapes

| # | Shape | Served by | Gate / anchor |
| --- | --- | --- | --- |
| S1 | single node, label + property predicates | scan-filter chain | `batch.rs:204-244`, preds `batch.rs:251` |
| S2 | single node, predicate outside the batch vocabulary (`CONTAINS`, functions) | walker | `batch.rs:284-301` |
| S3 | bare count over a label | count stats | `exec/count_stats.rs:249` (exact base stats, one empty seed row) |
| S4 | 1 fixed hop | walker | chain floor `matcher.rs:314` (2..=4) |
| S5 | 2–4 fixed hops, distinct node vars, static rel types, one predicate per node var | chain — *conditional* | `matcher.rs:312-358`; gate `match_clause.rs:99` |
| S6 | 2–4 hops with a rel property map / rel `WHERE` / var-length / dynamic type expression | walker | `matcher.rs:184-196` |
| S7 | 5+ fixed hops | walker | `MAX_CHAIN_HOPS = 4`, `matcher.rs:51`, `:314` |
| S8 | cyclic single pattern (a node variable repeats) | walker | `matcher.rs:319-326` (each node var once) |
| S9 | named path (`MATCH p = …`) | walker | `matcher.rs:157` (`has_path_name`) |

### 2.2 Multi-pattern shapes

| # | Shape | Served by | Gate / anchor |
| --- | --- | --- | --- |
| S10 | 2 patterns, 1–2 hops each, one shared hub at an end | hub-star — *conditional* | `matcher.rs:372-512`, sides `2..=3` nodes |
| S11 | S10 plus a second shared variable at the opposite ends (bounded cycle) | hub-star, `shared_terminal` | `matcher.rs:430-454`, `:124` |
| S12 | 2 patterns with a 3+-hop side, or 3+ patterns | walker | `matcher.rs:382`, `compile` `matcher.rs:144-148` |
| S13 | 2 patterns with no shared variable (cartesian) | walker | `matcher.rs:430-432` |
| S14 | 3+ patterns sharing a hub | walker; the exists side walks the DFS probe | `matcher.rs:147-148`; `exists_probe.rs:120` |

### 2.3 Quantified / variable-length shapes

| # | Shape | Served by | Gate / anchor |
| --- | --- | --- | --- |
| S15 | exactly one quantified element, node…node boundary, pair-shaped answer | frontier-NFA | `frontier_run.rs:310`, `nfa.rs:205`; answer `frontier_match.rs:432` |
| S16 | nested quantifiers, multiple quantified elements, per-state predicates (auto rung) | walker (GPM DFS) | `nfa.rs:231-254`, `frontier_match.rs:342-391` |
| S17 | unbounded `REPEATABLE ELEMENTS` (WALK pool) | authored rejection | ADR-0478 §5 |
| S18 | explicit TRAIL / SIMPLE / ACYCLIC over a quantified pattern | walker | `frontier_match.rs:287-301` |
| S19 | full enumeration answer (no `LIMIT`, no `DISTINCT`, no foldable aggregate) | walker | `frontier_match.rs:439-448`, `:520-525` |
| S20 | path count (`count(p)`, `count(*)`) | walker; bounded unfiltered WALK counts ride fold B | `frontier_match.rs:486-497`; `frontier_count.rs:54` |
| S21 | fixed-length concatenation under a `LIMIT` (no quantifier) | walker | `single_quantified` `frontier_run.rs:310` requires one quantified element; chain declines at `matcher.rs:157` if the path is named |

### 2.4 Non-pattern shapes

| # | Shape | Served by | Gate / anchor |
| --- | --- | --- | --- |
| S22 | `shortestPath` / `allShortestPaths`, selectors | GPM shortest machinery | `matcher.rs:157`, `nfa.rs:231-254` |
| S23 | `OPTIONAL MATCH` | walker (null-binding arm) | `kernels/mod.rs:207`, `frontier_run.rs:81` |
| S24 | mid-statement `MATCH` (carrying row / bound variables) | walker | `matcher.rs:141`, `mod.rs:207`, `frontier_run.rs:81` |
| S25 | writes (`CREATE`/`MERGE`/`SET`/`DELETE`) and `SEARCH` | write path / `search_match_clause` | `dispatch.rs:295-314`, `:213` |

Counted: **6 of 25 classes** have a compiled-kernel owner (S1, S3, S5, S10,
S11, S15). Of those, S5/S10/S11 are additionally conditional on §1.1 and the
cost gate; S3 serves only the whole-population count.

### 2.5 The cost gates

- chain: declines when any hop's estimated width exceeds
  `MAX_ESTIMATED_STAGE_ROWS = 1 << 20` (`kernels/mod.rs:408`, `:456-461`).
- hub-star: declines unless hub-driven work ≤ terminal-driven work (×2 in
  existence mode, `kernels/mod.rs:464-516`). The "dear hub plan" refusal is
  pinned at `kernels_tests/gate.rs:60`.
- frontier: engages on an unbounded quantifier, or when estimated paths reach
  4× estimated distinct endpoints (`frontier_match.rs:711`, `:745-776`).

### 2.6 Quarantined research kernels (not engine behaviour)

`relate/relate-executor/src/graph/sol.rs` holds three hand-written
speed-of-light kernels — `q19` (`sol.rs:168`), `q8` (`sol.rs:307`), `q30`
(`sol.rs:387`) — i.e. three fixed shapes of the graph-benchmark-ldbc
workload, not a general inventory. They compile only under the non-default
`research` feature (`graph/mod.rs:42-51`, `relate-executor/src/lib.rs:99-103`,
`relate/src/lib.rs:104-108`); no CLI bundle or the WASM build enables it
(`relate-executor/Cargo.toml:16`, `relate/Cargo.toml:17`).

The PathDB prototype `graph/qpp.rs` no longer exists: ADR-0478 deleted it and
productionized its semantics as the frontier family. Its structural
successors are the production NFA families at `exec/kernels/nfa.rs:25-40` —
flat (`-[r:T*m..n]-`, type unions), group (`((a)-[:A]-(b)-[:B]-(c)){m,n}`),
concatenation context (fixed sibling rels around the quantified element) and
nested quantifiers (bounded unroll, never cycles).

## 3. Corpora

In-repo corpora are the query-latency workloads (`testing/relate-query-latency/workloads/`).
The suite's LDBC id range is `src/workload.rs:18` (`1..=30`); the lane
registry is `src/corpus.rs:139`; every query file carries one machine-read
`// status:` marker (ADR-0479).

| Corpus | Queries | Served today (marker) | Note |
| --- | --- | --- | --- |
| graph-benchmark-ldbc SF0.1 | 30 | n/a (no markers; answer-checked) | all read-only, all fixed hops, no var-length, no shortestPath |
| pathdb | 142 | 100 ok / 42 kept | every statement is `MATCH p = … RETURN p LIMIT 100` (29 files hold a `UNION` of two; 171 statements) |
| pathdb restrictors | 65 | 34 ok / 24 kept / 7 error-pin | 13 templates × 5 path modes |
| snb-bi | 16 | 11 ok / 5 kept | 5 kept are unbounded `replyOf*0..` |
| agg-stress | 5 | 5 ok | bounded `knows*1..3` folds |
| fraud-qpp | 6 | 3 ok / 3 kept | planted-ring / bipartite classes |
| ukrailway | 2 | 2 ok | `+` chain, `ANY SHORTEST` |

PathDB shape split (measured over the 142 files): **106 quantified**, **36
fixed** (18 one-hop, 12 two-hop, 6 three-hop). The 42 `kept` entries are all
in the quantified set.

**Not in-repo: the traverse-bench / pokec query list.** No file enumerates
it. What exists is a synthetic pokec-*shaped* fixture
(`testing/relate-test-support/src/pokec.rs:54-75`) and three representative
query classes in `relate/relate/tests/pokec_fixture.rs:28-53`. The 11-shape
table in LABS-1119 is therefore the only record of the bench's shapes, and
**its per-query frequencies do not exist in this repository** — the 75%
denominator for that corpus cannot be computed today (candidate 6).

## 4. Coverage today

### 4.1 graph-benchmark-ldbc, query by query

Classification by the branches in §1; not executed.

| qid | Shape | Deciding branch | Served by |
| --- | --- | --- | --- |
| q01 | 2×1-hop, hub `p` | sink projection | walker |
| q02 | 1 hop | S4 floor + sink projection | walker |
| q03 | 2×1-hop, hub `person` | sink projection | walker |
| q04 | 1 hop | S4 floor + sink projection | walker |
| q05 | 1 hop, `DISTINCT`+`LIMIT` | distinct cap + sink projection | walker |
| q06 | 1 hop | S4 floor + sink projection | walker |
| q07 | 2 patterns, 3-hop side | S12 + sink projection | walker |
| q08 | 1 hop, `DISTINCT` | S4 floor + sink projection | walker |
| q09 | 1 pattern, 3 hops | **sink projection only** | walker |
| q10 | 2 patterns, 2+1 hops | **sink projection only** | walker |
| q11–q19 | 1–2 hops / 2×1-hop, aggregate | fold D (`agg_fold/plan.rs:349`) | path agg fold |
| q20 | 0-hop scan, `CONTAINS`+`length`, count | no fold shape; `CONTAINS` outside `compile_pred`; chain floor | walker |
| q21 | 1 hop, `COUNT(x) > 0` (not DISTINCT) | fold A needs `DISTINCT` (`exists_probe.rs:322`); fold D applies | path agg fold |
| q22 | 3 patterns, `count(DISTINCT)>0` | fold A; kernel declines 3 patterns | exists DFS probe |
| q23 | 2-hop chain, `count(DISTINCT)>0` | fold A → `try_exists_kernel` → chain arm | chain (exists) — cost-gate unverified |
| q24 | 2×1-hop, `count(DISTINCT)>0` | fold A → hub-star exists arm | hub-star (exists) — likely declined by the dear-hub gate |
| q25 | 2-hop chain | as q23 | chain (exists) — cost-gate unverified |
| q26 | 2×1-hop | as q24 | hub-star (exists) — likely declined by the dear-hub gate |
| q27 | 3 patterns | fold A; kernel declines | exists DFS probe |
| q28 | 3 patterns | fold A; kernel declines | exists DFS probe |
| q29 | 1 hop, `COUNT(x) > 0` (not DISTINCT) | fold D | path agg fold |
| q30 | 1+2 hops, shared hub and terminal | fold A → hub-star with `shared_terminal` | hub-star (exists) — cost-gate unverified |

Totals: **10/30 are closed by the sink gate before any structural check**
(q01–q10); of those, **4 are kernel-family shapes blocked by nothing else**
(q01, q03, q09, q10). Of the remaining 20: 11 path agg folds (q11–q19, q21,
q29), 3 exists DFS probes (q22, q27, q28), 1 walker (q20), and 5 whose
routing depends on the exists cost gate (q23–q26, q30).

By the shape of that gate, q23/q25 (chains, no hub-versus-terminal
comparison) engage; q24/q26 are the "dear hub plan" the gate exists to
refuse — selective indexed terminals against a fanning hub, the case pinned
at `kernels_tests/gate.rs:60` — and most likely keep the DFS probe; q30
(shared hub and terminal, no selective terminal) is marginal.

Compiled-kernel coverage of GBL is therefore **≈2–5/30 (7–17%)**, or
**≈13–16/30 (43–53%)** if the fold tier counts as compiled operators. Both
ranges are code-path predictions; the spread is the unverified cost gate.

### 4.2 PathDB

The 142 translated queries are all capped-emission QPP shapes, which is the
frontier's `Consumer::Capped` (`frontier_match.rs:511-530`). The 42 `kept`
markers were recorded at the 2026-11-01 spike, before the frontier landed;
ADR-0478's Consequences assert the frontier answers them at millisecond
scale. **Whether the markers have been re-recorded at HEAD is unverified** —
flipping them requires the opt-in SF0.1 corpus run (ADR-0479 gates it behind
`RELATE_QUERY_LATENCY`). If they hold, kernel coverage of PathDB could reach
the 106/142 quantified class (74.6%), modulo the NFA structural gates (S16);
the 36 fixed concatenations (S21) keep the walker.

## 5. Candidate list, ordered

Ordered by corpus frequency × serving cost. "Unlocks" is stated per corpus
and is a prediction, not a measurement.

1. **Sink-projection rider on the contracted kernels.** Let rung 3 apply the
   sink projection the way rung 1 already does (`frontier_run.rs:110-116`
   builds `MatchReduce` and calls `reduce_row`; `pattern_policies.rs:535`
   applies the clause filter and `finish_match_row`), then clear the
   `projection.is_none()` half of `match_clause.rs:99`. This is the LABS-1118
   root cause. Unlocks: GBL q01, q03, q09, q10 (+4); the pokec-bench
   `expansion_2/3/4`, `neighbours_2`, `scan_expand*`, `pattern_long/cycle`
   families (LABS-1119's candidates 1–4, 6, 9). Highest frequency in every
   corpus, medium cost. Correctness interlock: `dispatch.rs:373` must still
   see `SinkProjected`, so the kernel has to apply the projection, not just
   ignore the gate. Filed as LABS-1123 (kernel composition).
2. **Text predicates in the closed vocabulary** (`CONTAINS`, prefix/suffix).
   `decode_conjunct` (`kernels/program.rs:409-442`) and the batch
   `compile_pred` (`batch.rs:251-303`) accept only comparison and null
   checks; `CONTAINS` appears in 7 of the 30 GBL queries (q02, q05, q06, q16,
   q19, q20, q29). Unlocks the walker-served ones among them once their
   pattern shape is also served. Needs one `NodePred` implementation plus a
   matcher arm — the ADR-0473 extension point, not a new mechanism.
3. **One-hop chain** (lower the floor at `matcher.rs:314` to 1). Unlocks S4:
   GBL q02, q04, q06, q08 (+4 once candidate 1 lifts the projection gate);
   the pokec single-hop / `neighbours_1` family. Low cost — the same unrolled
   loop with one hop; the value is columnar batching, not fewer checks.
4. **N-pattern star** (generalize hub-star from exactly two patterns to 3+
   sharing one hub; wire it into the exists entry as well). Unlocks S14: GBL
   q22, q27, q28 (+3), currently DFS probes. Medium cost.
5. **Frontier for fixed-length concatenation under a cap** (S21). The PathDB
   `(A.B)` / `((A.B).C)` classes — 36 queries — run the walker today and are
   `ok`, so this is a constant-factor play, not a timeout fix. Medium cost,
   defer behind 1–4.
6. **Capture the pokec/traverse-bench query list in-repo.** Without it the
   75% target has no denominator for the corpus the ticket measures. Cheap;
   a prerequisite for any honest coverage claim.
7. **Re-record the PathDB corpus at HEAD** (`corpus --seed-policy median`,
   ADR-0479). Decides whether the 42 `kept` markers are stale. Cheap, and it
   is the only way to know what the frontier actually took.
8. **Engagement observability** (LABS-1117). Coverage cannot be counted from
   the query surface today; PROFILE carries no per-rung engagement fact. This
   is the ticket's own success criterion — every percentage in this note is a
   code-path prediction until it lands.

Projected on GBL if 1–4 land and the cost gates permit: compiled traversal
kernels go from ≈2–5 to ≈12–15 of 30 (40–50%); counting the fold tier as
well, ≈23–26 of 30 (77–87%). Whether the same holds for the pokec bench is
unknown until candidate 6 exists.

## 6. Structurally hopeless for a compiled kernel

These shapes cannot be served by a deduplicating/columnar kernel without
changing answers, or the machinery already owns them:

- **Full path enumeration** without a cap, `DISTINCT`, or a foldable
  aggregate (S19). Distinct trails are distinct answers; BFS dedup collapses
  them (ADR-0478 §2).
- **`ORDER BY` + `LIMIT`**: a capped kernel emits a contract-free prefix,
  which sorting forbids (`frontier_match.rs:525`).
- **Path counts** `count(p)` / `count(*)` over a quantified shape: the answer
  counts paths, not endpoints (`frontier_match.rs:486`).
- **Selectors** `ANY`/`ALL SHORTEST` (S22) — the GPM shortest machinery owns
  them.
- **Writes** (S25) — not an answer shape.
- **Dynamic label/type expressions**, rel property maps (S6) — the walker's
  evaluation.
- **Cartesian products and OPTIONAL's null arm** (S13, S23).
- **Carrying rows** (S24): per-row semantics are the walker's by definition.

## 7. What is unverified

- No query was executed; every "served by" is read from the branch that
  decides it. The hub-star and chain cost gates can decline any of q23–q26
  and q30 regardless of shape.
- The PathDB `kept` markers predate the frontier (ADR-0478); whether the
  frontier serves those 42 today is not recorded at HEAD.
- The pokec/traverse-bench query list and its frequencies are not in this
  repository; LABS-1119's gap ratios (19×/52×/347×, …) come from an external
  bench run and were not re-measured here. They are throughput-qps ratios; the
  4-hop figure is the unrounded 347× (an earlier 312× derived from dividing by
  a rounded qps print).
- The 75% goal has no agreed denominator. Per corpus it means different
  things: on GBL it is reachable (candidates 1–4), on PathDB the
  full-enumeration classes cap it, and on the pokec bench it is currently
  unmeasurable (candidate 6).

## References

ADR-0468 (batch substrate), ADR-0471 (one var-length engine), ADR-0473
(contracted traversal kernels), ADR-0476 (research-kernel quarantine),
ADR-0478 (frontier-NFA), ADR-0479 (QPP/LDBC corpus), ADR-0519 (diagnostic
pattern-engine switch), ADR-0521 (sparse frontier tables), ADR-0523 (walk
count), ADR-0524 (plain cap rider). Sibling tickets: LABS-1116 (per-element
visibility tax), LABS-1117 (engagement observability), LABS-1118 (NFA for
the regular pattern space), LABS-1123 (kernel composition).
