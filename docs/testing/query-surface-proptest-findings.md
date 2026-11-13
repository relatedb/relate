# Property / Metamorphic / Differential Testing of Query Languages — Findings for Relate

Scope: what the literature says about testing **query languages** (SQL first, then graph/Cypher) for
*logic bugs* — wrong results that do not crash — and how to apply it to Relate's Cypher surface.
Companion to `proptest-research-findings.md` (Rust harness mechanics); this file is about
*generation strategies, oracles, and bug classes* for the query layer.

All numbers are as reported by the cited papers. Where the literature is thin (Cypher-specific
testing, operator-directed generation) this is stated explicitly.

---

## 0. TL;DR for Relate

- The SQLancer **oracle family** (TLP, NoREC, PQS, DQP, QPG) is the canonical, best-evidenced
  body of work. Each oracle detects logic bugs **without a reference implementation** by
  constructing a second query whose result is *provably* related to the first.
- **TLP (Query Partitioning)** is the single highest return-on-effort oracle: no reference engine,
  ~200 LOC, works on WHERE / aggregation / DISTINCT / OPTIONAL. It has a **direct graph analogue**
  (GDBMeter, ISSTA 2023) that found 40 bugs in Neo4j/JanusGraph/TigerGraph — 14 logic bugs,
  **no false alarms**.
- The **graph-DB testing line** (GDsmith → GDBMeter → Gamera → GraphGenie → GRev → GraspDB →
  QuDi → PATHTest) is the Cypher-specific analogue. It is real and productive but **every tool
  targets server engines (Neo4j, RedisGraph, Memgraph, JanusGraph), not an embedded engine**, and
  most use differential or metamorphic oracles, not a model-based proptest harness.
- **Differential testing against Neo4j** is attractive because the `neo4rs` 0.8.0 client
  crate covers the Bolt side. But the literature is blunt: cross-engine
  differential testing on query languages produces **high false-alarm rates** (Grand: 615/1000
  candidate divergences, all 30 sampled were false alarms; GraphGenie measured Grand's false-alarm
  rate at >80%). Dialect divergence, not bugs, dominates.
- **The user's operator-driven idea** ("reverse-generate statements from planner operators") has a
  *partial* precedent — **QPG** drives generation by *plan* coverage, **ParSEval** targets *plan
  branch* coverage, **Kangaroo** enumerates *all* plans as an oracle — but **no published tool
  inverts operators into query fragments**. It is plausible but expensive and weakly supported;
  plan-coverage feedback (measure plans, steer forward generation) is the better-supported version
  of the same instinct.
- **Recommended first builds for Relate:** (1) TLP-style query partitioning over a
  generator seeded by the 711 TCK `.feature` files; (2) a set of Cypher metamorphic relations
  (direction reversal, undirected ≡ union of directions, variable-length ≡ union of fixed lengths,
  WITH-rename transparency, OPTIONAL neutralization, LIMIT/OFFSET windowing). Both need no
  reference engine and reuse the existing `proptest` infrastructure.
- **Do not build first:** PQS (per-operator typed synthesis, the most expensive oracle), a
  from-scratch grammar fuzzer (the TCK corpus already is a validity-preserving seed pool), or
  operator→query inversion (Relate's `EXPLAIN` prints clause names only — plan-level coverage
  is not currently measurable).

---

## 1. SQLancer's oracles (Rigger et al.)

The reference body of work. Manuel Rigger's DBMS bug list (<https://www.manuelrigger.at/dbms-bugs/>)
tallies hundreds of logic bugs found by these techniques across SQLite, MySQL/MariaDB, PostgreSQL,
CockroachDB, TiDB, DuckDB and others. Each oracle solves the test-oracle problem *differently*, and
each catches a different bug class — the papers are explicit that the oracles are **complementary,
not ranked**.

### 1.1 TLP — Ternary Logic Partitioning (Query Partitioning)

- Paper: *Finding Bugs in Database Systems via Query Partitioning*, OOPSLA 2020,
  <https://www.manuelrigger.at/preprints/TLP.pdf> (DOI 10.1145/3428279).
- **Mechanism.** Pick a predicate `p` (usually random). Decompose a query `Q` into three queries
  that filter on `p`, `NOT p`, and `p IS NULL`; compose their results (UNION for WHERE) and require
  the composed result to equal `RS(Q)`. This rests on SQL/Cypher **ternary logic**: every row
  satisfies exactly one of the three, so the partition is total. Applies to WHERE, GROUP BY, HAVING,
  aggregates, and DISTINCT.
- **Why it finds bugs.** The partitioning queries are *more complex* than `Q` and push the optimizer
  down different paths (different index usage, different predicate pushdown), so a wrong
  optimization shows up as an inconsistent union. No reference engine needed.
- **Result.** 175 bugs in MySQL/TiDB/SQLite/CockroachDB, 125 fixed; **77 were logic bugs**.
- **Cypher applicability.** High. Cypher is also 3-valued (`NULL` propagates; `WHERE` keeps only
  `TRUE`). `p IS NULL` is valid Cypher. Caveats: Cypher's **relationship-uniqueness** rule (a
  relationship may not be traversed twice in one MATCH) means a naive `Q` vs `Q_partitioned` rewrite
  can change the multiplicity of paths, so partitioning must be applied to a predicate over a
  *fixed* pattern, not by duplicating the pattern. Also `OPTIONAL MATCH` introduces NULLs that
  interact with the `p IS NULL` partition in ways the relational formulation does not cover.

### 1.2 NoREC — Non-Optimizing Reference Engine Construction

- Paper: *Detecting Optimization Bugs … via Non-Optimizing Reference Engine Construction*,
  ESEC/FSE 2020, <https://ar5iv.labs.arxiv.org/html/2007.08292> (DOI 10.1145/3368089.3409710).
- **Mechanism.** Rewrite `SELECT * FROM t WHERE φ` into `SELECT (φ IS TRUE) FROM t`. Moving the
  predicate into the projection removes the WHERE clause, so the engine "must fetch every record"
  and most optimizations cannot apply. The two results must agree in count (and content).
- **Result.** 51 logic bugs (SQLite 39, CockroachDB 7, MariaDB 5, PostgreSQL 0) plus 58 error bugs;
  SQLite alone 110 bugs. Implementation "consists of less than 200 LOC."
- **Comparison.** NoREC found 52.7% of PQS's bugs and vice versa; NoREC uniquely caught duplicate-row
  and aggregate-counting bugs.
- **Cypher applicability.** Medium. Relate has no separate boolean column type to project into
  (`φ IS TRUE` is not a standard Cypher expression), so the literal SQL trick does not port.
  The *idea* — evaluate the predicate per row in a form the optimizer cannot push down, and compare
  counts — ports via `WITH` / `RETURN` and a `count()` of matching rows.

### 1.3 PQS — Pivoted Query Synthesis

- Paper: *Testing Database Engines via Pivoted Query Synthesis*, OSDI 2020,
  <https://www.usenix.org/system/files/osdi20-rigger.pdf>.
- **Mechanism.** Generate a database, pick a random **pivot row**, then *synthesize a query that is
  guaranteed to fetch that row* (build the WHERE/JOIN expression by evaluating it against the pivot
  and forcing each sub-expression TRUE). If the DBMS fails to return the pivot row, that is a bug —
  the **containment oracle**. No reference implementation.
- **Why it is strong.** It finds optimizer bugs that only manifest for specific rows and index
  choices (e.g. the SQLite partial-index bug where `c0 IS NOT 1` was assumed to imply `c0 NOT NULL`).
- **Result.** 121 unique bugs in SQLite/MySQL/PostgreSQL, 96 fixed/verified.
- **Cost.** High. "a single comparison operator alone covers already more than 200 LOC" of
  expression synthesis — PQS requires a typed expression generator that can *invert* a desired
  truth value into an expression, for every operator.
- **Cypher applicability.** Medium-high in principle, high in cost. The pivot row is a node or
  relationship; the synthesized query is a `MATCH` pattern + `WHERE` guaranteed to bind it.

### 1.4 DQP — Differential Query Plans

- Paper: *Keep It Simple: Testing Databases via Differential Query Plans*, SIGMOD 2024,
  <https://bajinsheng.github.io/assets/pdf/dqp_sigmod24.pdf> (DOI 10.1145/3654991).
- **Mechanism.** Force the *same query* through *different query plans* (via DBMS-specific hints)
  and compare results. Found that 14 of 15 unique bugs in the prior TQS approach were actually
  plan-discrepancy bugs, so DQP just does that directly.
- **Result.** Reproduces 14/15 TQS bugs; **26 previously unknown unique bugs**; <100 LOC per system.
  **81% of DQP's logic bugs were not found by NoREC or TLP, and DQP missed 86% of theirs** — strong
  evidence the oracles are complementary.
- **Cypher applicability.** Low as written (Relate has no plan hints / forced-plan facility), but
  the *multi-plan* idea reappears in Kangaroo (see §4) as "execute all plans, compare."

### 1.5 QPG — Query Plan Guidance

- Paper: *Testing Database Engines via Query Plan Guidance*, ICSE 2023,
  <https://arxiv.org/html/2312.17510v1>.
- **Mechanism.** Treat the query **plan** (physical operator tree, obtained via `EXPLAIN`) as a
  black-box coverage signal. Collect unique plans into a **plan pool** (hash table plan→query). When
  no new plan appears after N queries, **mutate the database state** (DDL/DML) — not the query,
  because the test oracles constrain query shape. Operators are selected by a multi-armed bandit
  (ε=0.7). Pair with TLP/NoREC.
- **Result.** **53 unique bugs** (SQLite 28, CockroachDB 16, TiDB 9), 35 fixed; 4.85–408× more unique
  plans than SQLancer; 2× more bugs and 1.4× more unique bugs than SQLancer. New-bug plans averaged
  **6.35 operators** — 2.47× longer than prior-known-bug plans. ~1000 LOC Java + ~100/DBMS.
- **Cypher applicability.** This is the *closest existing work* to the user's operator idea (see §4),
  but note it is **forward generation + plan feedback**, not inverse generation from operators.

### 1.6 Scaling and LLM oracles (context for effort)

- **SQLancer++** (*Scaling Automated Database System Testing*, <https://arxiv.org/html/2503.21424v2>):
  a DBMS-agnostic generator with Bayesian feature inference; 196 bugs across 18 DBMSs, 180 fixed,
  140 logic (TLP 133, NoREC 7). 8.4K LOC vs SQLancer's 83K. Adding a DBMS averaged 16 LOC. Explicitly
  notes that oracles needing DBMS-specific artifacts (DQP's hints, CERT's plans, Mozi's configs)
  are the ones that resist scaling.
- **Argus** (*Automated Discovery of Test Oracles for DBMS Using LLMs*, <https://arxiv.org/html/2510.06663v2>):
  LLM-generated "equivalent CAQ pairs" (constrained abstract queries), formally verified by an
  SMT-style prover (SQLSolver), then instantiated. **41 bugs, 36 logic, 36 confirmed, 27 fixed**;
  claims 3.33× more unique logic bugs than the sum of 11 prior oracles (TLP, NoREC, EET, DQP). Key
  caution: replacing the formal prover with an LLM-as-judge produced **20/20 false positives** —
  oracle soundness must be machine-checked, not judged.

**Which found the most?** For a single oracle, TLP/NoREC/PQS each report 51–121 logic bugs over
multi-year campaigns; QPG found 53 mostly *new* bugs (in already-heavily-tested engines) with plan
guidance. No source claims one oracle dominates; every comparative paper reports low overlap.

---

## 2. Graph / Cypher-specific query testing

This is the Cypher analogue and the most directly transferable literature. All tools target
**server** graph DBMSs; none targets an embedded openCypher engine, and none mutates a TCK corpus.

| Tool | Venue | Generation | Oracle | Systems | Bugs (logic) |
|---|---|---|---|---|---|
| **Grand** | ICSE 2019 | random Gremlin | **differential** (cross-engine) | 6 GDBMS | 21 (2) — but see false alarms |
| **GDsmith** | ISSTA 2023 | 2-stage Cypher skeleton grammar + context typing + 3 structural mutations | **differential** (cross-engine / cross-version / cross-optimization) | Neo4j, RedisGraph, Memgraph | 27 (18 wrong-result) |
| **GDBMeter** | ISSTA 2023 | Cypher generator | **Query Partitioning (TLP) for graphs** | Neo4j, JanusGraph, TigerGraph | 40 (14), **0 false alarms** |
| **Gamera** | VLDB 2024 | Cypher query patterns | **graph-aware metamorphic relations** | 7 GDBs (Neo4j, RedisGraph, OrientDB, JanusGraph, HugeGraph, TinkerGraph, ArcadeDB) | 39 (mostly logic), 15 confirmed |
| **GraphGenie** | ICSE 2024 | base Cypher/Gremlin query + **injective/surjective graph query transformation (GQT)** | metamorphic (⊇/⊆/≡ relations on result sets) | 6 GDBMS | 25 (16 logic, 6 perf, 3 errors) |
| **GRev** | ICSE 2024 | **Abstract Syntax Graph** + Random Walk Covering → equivalent rewrites | metamorphic (equivalent query) | GDBMS | 22 (12 logic, 3 perf, 4 missing-exception) |
| **GraspDB** | ISSTA 2024 | mutation rules for **write** queries | **Graph-State Persistence oracle** (LPG-/LPSG-isomorphism) | 4 engines | 77, 58 confirmed (31 write-related) |
| **QuDi** | ISSTA 2024 | disassemble a Gremlin query into atomic traversals | metamorphic (disassembled ≡ original) | 6 GDBs | 25 logic, 10 confirmed |
| **Dinkel** | VLDB 2026 | **state-aware** clause-by-clause Cypher generation (query context + graph schema) | *no logic oracle* — ASan / errors / assertions only | Neo4j, RedisGraph, Apache AGE | 60 (crash/error only), 58 confirmed |
| **PATHTest** | VLDB 2026 | iterative non-empty variable-length query generator + 3 transformation rules | metamorphic (fixed-length ≡ variable-length) | 7 GDBMS | 41 (24 logic, 17 errors) |
| **GraphQE** | ICDE 2025 | — | **Cypher equivalence prover** (SMT, bag semantics) | (verifier, not fuzzer) | n/a |

### 2.1 GDsmith (the canonical Cypher generator)

*GDsmith: Detecting Bugs in Cypher Graph Database Engines*, ISSTA 2023,
<https://taoxiease.github.io/publications/issta23-gdsmith.pdf> (also <https://arxiv.org/html/2206.08530>).

- **Generation.** Two stages. (1) A **skeleton grammar** mirroring core Cypher
  (MATCH/OPTIONAL MATCH/WITH/UNWIND/RETURN/WHERE) produces a clause sequence with uninstantiated
  parts. (2) Uninstantiated patterns/expressions are filled using per-clause **context** (variables
  in scope, their types, label/type constraints) enforcing three semantic requirements: *variable
  scope correctness*, *operand type safety*, *property key safety*. This yields **100% semantic
  validity and 98% grammar coverage**, versus 16% semantic validity for a syntax-only baseline.
- **Mutations.** DelayReturn, AdvanceReturn, RemoveCondition, applied to a pool of non-empty-result
  queries; property keys are sampled with a frequency bias toward rare keys.
- **Oracle.** Differential only: cross-engine, cross-version, cross-optimization. **No TLP, no
  metamorphic equivalence.**
- **Result.** 27 previously-unknown bugs (Neo4j 7/8, RedisGraph 2/5, Memgraph 0/5 as error/wrong
  result); 14 confirmed, 7 fixed (the paper's own table sums differently — a documented discrepancy).
- **Relevance to Relate.** The two-stage skeleton+context typing approach is the state of the art
  for *valid* Cypher generation and is the natural template for a Relate generator. Its weakness
  (no oracle beyond differential) is exactly the gap TLP/metamorphic fill.

### 2.2 GDBMeter (Query Partitioning for graphs — the direct TLP analogue)

*Testing Graph Database Engines via Query Partitioning*, ISSTA 2023,
<https://matteokamm.ch/issta2023_paper.pdf> (DOI 10.1145/3597926.3598044).

- Applies TLP's partitioning idea to graph queries: derive queries whose combined result must
  reconstruct the original. **40 unique bugs, 14 logic, 35 confirmed, 27 fixed.**
- Directly compares to Grand (differential): for 1000 random queries Grand flagged **615** candidate
  divergences; a random sample of 30 were **all false alarms**. GDBMeter reported logic bugs with
  **no false alarms**.
- **This is the strongest single evidence that a partitioning oracle beats differential testing for
  graph query languages**, because it is dialect-independent and reference-free.

### 2.3 Gamera (graph-aware metamorphic relations)

*Testing Graph Database Systems via Graph-Aware Metamorphic Relations*, VLDB 17(4):836, 2024,
<https://www.vldb.org/pvldb/vol17/p836-zhuang.pdf>.

Three classes of MRs; **39 bugs across 7 GDBs, 15 confirmed, 3 fixed**, in ~48h:
- **Elementary (node/edge level):** spouse (two nodes sharing a child — set-inclusion both ways),
  ancestor/descendant (inverse relation), k-hop neighbour symmetry (outgoing k-hop of A contains B
  ⟺ incoming k-hop of B contains A), edge-property relations.
- **Path level:** path-number calculation (paths A→C via B must equal (A→B)×(B→C)), shortest path.
- **Compound:** **pattern fusion** (union of two patterns ≡ union of their matches — this single MR
  found **30 of 39 bugs**), pattern partitioning.
- **Dynamic:** data addition/deletion/update, irrelevant-data manipulation.

Pattern fusion dominating the yield is important: *the simplest structural equivalences find the most
bugs*. Gamera's limits: it does not find bugs shared by all engines, and it needs hand-written
per-MR query templates.

### 2.4 GraphGenie / GRev (equivalent-transformation oracles)

- **GraphGenie** (ICSE 2024, <https://yuanchengjiang.github.io/docs/GraphGenie-ICSE24.pdf>): mutates
  the **G-Pattern** (the directed edge set) of a base query via three operators — `○=` (equivalent),
  `○>` (superset), `○<` (subset) — derived from injective/surjective mappings of edge sets. Result-set
  relations are checked (`RS(Q') ⊇ RS(Q)`, etc.). **25 bugs (16 logic, 6 perf, 3 internal errors)**,
  and found **6 logic bugs GDBMeter could not**. Explicitly claims **zero false alarms**, versus
  Grand's >80%.
- **GRev** (ICSE 2024, <https://joyemang33.github.io/assets/pdf/GRev.pdf>): encodes the base query's
  semantics into an **Abstract Syntax Graph**, then finds non-overlapping paths covering it (Random
  Walk Covering) to emit an equivalent query that produces a *different plan*. **22 bugs (12 logic,
  3 perf, 4 missing exceptions, 3 unexpected exceptions), 15 confirmed**, 14 graph-related.
- These are the cleanest examples of **Cypher query rewriting as an oracle** — directly reusable
  ideas for §5.

### 2.5 GraspDB (write-path oracle), QuDi, PATHTest

- **GraspDB** (ISSTA 2024, <https://xgdsmileboy.github.io/files/paper/issta24-db.pdf>): the first
  oracle for **writing operations** (CREATE/SET/DELETE/MERGE). Uses **Labeled Property Graph
  Isomorphism** and **Labeled Property Subgraph Isomorphism** as metamorphic relations — mutate the
  graph by writes, then check the queried graph is isomorphic to the expected one. **77 bugs, 58
  confirmed, 31 write-related.** Relevant because Relate's TCK + WAL work already touches writes.
- **QuDi** (ISSTA 2024, DOI 10.1145/3650212.3680392): disassemble a Gremlin query into atomic
  traversals and check the disassembled form returns the same result. 25 logic bugs, 10 confirmed.
- **PATHTest** (VLDB 19(9):2234, 2026, <https://www.vldb.org/pvldb/vol19/p2234-gui.pdf>):
  result-equivalent transformations between **fixed-length** and **variable-length** patterns
  (e.g. `*1..2` ≡ one-hop UNION two-hop). **41 bugs (24 logic, 17 errors)**, all beyond 7 existing
  tools. This is the most recent Cypher-relevant work and a direct source for §5.

### 2.6 GraphQE (Cypher equivalence prover — for oracle *soundness*)

*Proving Cypher Query Equivalence*, ICDE 2025, <https://ar5iv.labs.arxiv.org/html/2504.15742>.
A U-semiring/G-expression encoding of Cypher under **bag semantics**, checked with Z3.
Proved **138/148** equivalent Cypher pairs (Calcite-Cypher 73/80, LDBC 13/13, Cypher-for-gremlin
23/23, GraphDB-benchmarks 29/32), average 38 ms. Sound but **not complete** (fails on nested
aggregates, differing ORDER BY/LIMIT/SKIP). Its **Table II rewrite rules are a ready-made,
machine-checked list of Cypher equivalences** — see §5. Use it to *validate* that a metamorphic
relation is sound before trusting it as an oracle (cf. Argus's LLM-as-judge false-positive disaster).

### 2.7 Gap in the literature

- No published Cypher testing tool targets an **embedded** engine (all are client-server).
- No published tool uses a **conformance corpus (TCK) as a seed pool** for mutation; generators are
  grammar-based (GDsmith, Dinkel) or pattern-template-based (Gamera).
- Dinkel (VLDB 2026, <https://arxiv.org/html/2408.07525v2>) is the strongest *generation* work
  (state-aware, clause-by-clause, 93.4% validity, 60 bugs) but **deliberately uses no logic oracle** —
  it only catches crashes/errors/assertions. So even the best generator leaves the logic-bug oracle
  to other tools.

---

## 3. Differential testing against a reference engine

### 3.1 What the literature says

- **Slutz's RAGS (1998)** originated differential testing for DBMSs; **Grand** (ICSE 2019) applied it
  to Gremlin across 6 GDBMSs (21 bugs, 18 confirmed, 7 fixed, 2 logic).
- **The consistent finding is that differential oracles on query languages are noisy.** GDBMeter
  measured Grand at 615/1000 candidate bugs with a sampled 30/30 false-alarm rate; GraphGenie
  measured >80% false alarms. The cause is **semantic divergence**: engines implement different
  dialects and different behaviors for undefined/under-specified cases (NULL handling, ordering,
  integer overflow, floating point).
- **GDsmith** tames this by (a) using only deterministic clauses, (b) **skipping undefined behavior**
  (integer overflow, division by zero), (c) returning few expressions rather than all entities, and
  (d) excluding features unsupported by either engine. That is the practical recipe.
- **Turso's differential fuzzer** (<https://github.com/tursodatabase/turso/blob/main/.claude/skills/differential-fuzzer/SKILL.md>)
  compares generated SQL against **SQLite as the reference**, with an explicit non-fatal carve-out:
  "Unordered LIMIT mismatch" is not a bug because "LIMIT without ORDER BY may return different valid
  rows." Architecture: generator → runner (executes on both) → oracle (`oracle.rs`, compares row
  sets / error-vs-success / schema) → shrink to `minimized.sql`. Seeded, replayable, CI-runnable.
- **ACME** (NUS, <https://yuanchengjiang.github.io/docs/acme.pdf>): for *emerging* SQL-like systems,
  uses **LLMs to discover clause mappings** that translate system-specific features into equivalent
  SQL on a mature reference engine, then validates by execution. 59 bugs (17 logic, 42 internal
  errors), 52 fixed. Shows how to bridge dialect gaps for differential testing — but relies on a
  well-specified reference and on execution-validated mappings.
- **APOLLO** (<https://db.cc.gatech.edu/apollo-automated-testing-of-database-systems/>) and **Amoeba**
  are differential *performance*-bug tools (version-to-version), not logic-bug tools — different
  problem, mentioned only because "differential" is overloaded. **Thanos** (ICSE 2025,
  DOI 10.1109/ICSE55347.2025.00001) rotates storage engines under one engine as a differential axis.

### 3.2 Practical setup cost for Relate

- The `neo4rs` 0.8.0 client crate (with its `AuraAuth`/`Graph` types) covers the Bolt side.
  Running a local Neo4j container and driving both engines is
  therefore a wiring exercise, not new infrastructure.
- **The cost is not the transport; it is semantic divergence.** Relate targets the **openCypher
  TCK 2024.3** as its floor and supports CIP features (SEARCH, GROUP BY, conditional queries) that
  Neo4j 5/2025 implements differently or not at all. A raw differential run will drown in dialect
  differences.
- **Mitigation that the literature supports:** restrict the generated query subset to the *shared*
  TCK-passing core (the 3,892 allowlist scenarios are exactly a set of queries both engines should
  agree on), skip undefined behavior, and compare only deterministic queries without unordered
  LIMIT. Expect to triage a large candidate set by hand initially.

### 3.3 How differential and metamorphic oracles compose

They are orthogonal and should be **stacked**, not chosen between:
- A **metamorphic** oracle (TLP, equivalence rewrite) needs only *one* engine and is dialect-free; it
  is the primary oracle.
- A **differential** oracle needs a reference and is dialect-sensitive; use it as a *second* pass over
  the same generated corpus, and only on the shared dialect subset.
- GraphGenie's design is the template: generate base queries, apply *sound* transformations to get
  guaranteed result-set relations (metamorphic), and additionally cross-check engines (differential)
  where dialects allow.

---

## 4. Coverage guidance and the operator-driven idea

### 4.1 What exists

- **QPG (plan coverage).** §1.5. Measures unique physical plans via `EXPLAIN`, mutates DB *state* to
  reach new plans. This is *forward* generation with plan feedback — the best-evidenced use of plan
  information for query testing (53 new bugs in mature engines).
- **ParSEval** (*Plan-aware Test Database Generation for SQL Equivalence Evaluation*, VLDB 18(11):4750,
  2025, <https://www.vldb.org/pvldb/vol18/p4750-miao.pdf>): models each **query operator's behavior**
  and "considers all possible execution paths of the logical query plan by adapting the notion of
  branch coverage." Generates **test databases** to cover plan branches, to *disprove* query
  equivalence. "supports up to 40% more query pairs" than verification-based approaches and is "21×
  faster" than prior test-case-based approaches. This is the closest published work to "operator
  coverage" — but it targets *data* generation for equivalence checking, not query synthesis.
- **Kangaroo** (*Detecting DBMS bugs with context-sensitive instantiation and multi-plan execution*,
  Computers & Security 157:104564, 2025, <https://yajin.org/papers/cose2025_dbms.pdf>): "**multi-plan
  execution** makes the DBMS execute **all** query plans instead of the default optimal one, and
  compares the results." 54 bugs (41 crash, 13 logic). This is the practical, hint-free version of DQP.
- **SQUIRREL** (CCS 2020, <https://changochen.github.io/publication/squirrel_ccs2020.pdf>): an
  **intermediate representation (IR)** for SQL, type-based mutations (insert/delete/replace statement)
  that preserve syntactic validity, plus **logical dependency analysis** to preserve semantics, driven
  by **code-coverage feedback** (AFL-style). 51 SQLite + 7 MySQL + 5 MariaDB bugs, 52 fixed, 12 CVEs;
  2.4–243.9× higher semantic correctness than mutation fuzzers. This is the canonical
  "validity-preserving mutation + coverage feedback" baseline.
- **DBcover** (*White-box SQL Test Generation for Coverage*, ICSE 2026 industry track,
  <https://arxiv.org/abs/2608.25573>): builds a SQL-to-execution-path mapping and steers an LLM to
  generate SQL that "trigger[s] previously uncovered code regions." 80.1%/82.3% coverage on
  PostgreSQL/MySQL. Note: it targets **code** coverage, **not** plan/operator coverage.
- **Grammar-based generation (baseline).** SQLsmith (<https://github.com/anse1/sqlsmith>) generates
  random *valid* SQL by construction from the grammar; it is the ancestor of SQLancer's generators
  and of GDsmith's skeleton grammar. It finds crashes well, logic bugs not at all (no oracle).
  **Griffin** (grammar-free DBMS fuzzing) and **DynSQL** are later variants; **SQLRight**,
  **WingFuzz** add coverage feedback.

### 4.2 Honest assessment of "reverse-generate statements from planner operators"

The user's idea: instead of generating a query and observing which operators it produces, *start
from* a target operator (or operator tree) and synthesize a query that forces it, to guarantee
planner-path coverage.

**Precedent.** There is **no published tool that does literal operator→query inversion** for SQL or
Cypher. The nearest neighbors are:
- QPG — uses plans as a *feedback signal* for forward generation (not inversion).
- ParSEval — operator/branch coverage, but for *data* generation and equivalence checking.
- Kangaroo / DQP — force *all* plans at *execution* time, sidestepping the need to synthesize a query
  for a specific plan.
- Squirrel/DBcover — coverage feedback at the *code* level, not the operator level.

**Why it is attractive.** It is the only way to get *guaranteed* coverage of rare operators
(anti-joins, shortest-path, variable-length expansion, OPTIONAL, EXISTS, quantified pattern
predicates). Random generation covers common operators heavily and rare ones almost never; QPG's
"new-bug plans averaged 6.35 operators" shows bugs concentrate in complex plans.

**Why it is expensive and weakly supported.**
1. **The inverse mapping is many-to-many.** One physical operator (e.g. a hash join, a
   var-length expand) is produced by many query shapes; conversely one query shape maps to a plan only
   *after* optimization. There is no published catalogue of "query fragment → operator" for SQL, let
   alone Cypher. You would be writing and maintaining that catalogue by hand per operator.
2. **Coverage is not directly measurable in Relate today.** Per the project's own notes,
   `EXPLAIN` "prints only clause names, no operators," so QPG-style plan-pool feedback (the
   well-evidenced technique) cannot be implemented until the planner exposes a physical operator tree.
   This is a prerequisite, not a detail.
3. **The literature's own verdict is that plan-guidance works via *feedback on forward generation*,
   not inversion** (QPG), and that the cheapest way to force specific plans is at execution time
   (Kangaroo's multi-plan execution), which Relate could do internally without synthesizing
   queries.
4. **Validity/plannability risk.** An operator-derived query fragment may be semantically invalid
   (out of scope, wrong type) or may not actually produce the target operator after optimization —
   so you need a validator and a coverage check anyway, which is QPG again.

**Verdict.** The *instinct* — drive generation by planner/operator coverage — is well-supported
(QPG, ParSEval). The *specific mechanism* — inverse generation from operators — is novel, unproven,
and would be the most expensive component in the suite. If pursued, the lower-risk form is:
expose a physical plan tree from the planner → implement **QPG-style plan-pool feedback on forward
generation** → and only later consider operator-directed synthesis for the operators that random
generation provably under-covers. Treat it as a *later* step, and only after plan introspection
exists.

### 4.3 The baseline you should not skip

Before any coverage machinery: **grammar-based generation with structural validity + mutation**
(SQLsmith/Squirrel/GDsmith style), seeded from the TCK corpus. It is cheap, it is the substrate every
oracle above needs, and it is where GDsmith's 98% grammar coverage / 100% semantic validity comes
from.

---

## 5. Metamorphic relations for a graph query language

Concrete relations usable as Relate oracles, with the source that justifies each. Each is a
transformation `Q → Q'` with a *provable* result-set relation (equality, or superset/subset). They
need **one engine only** and no reference implementation.

**Direction and undirectedness**
1. **Direction reversal.** `(a)-[r]->(b)` ≡ `(b)<-[r]-(a)` with the returned columns mapped
   accordingly. (GraphQE rewrite rules; GDsmith generation symmetry.)
2. **Undirected ≡ union of directions.** `MATCH (a)-[r]-(b) RETURN a.name` ≡
   `MATCH (a)-[r]->(b) RETURN a.name UNION ALL MATCH (a)<-[r]-(b) RETURN a.name`. (GraphQE Table II;
   standard Cypher semantics.)

**Pattern and path length**
3. **Variable-length ≡ union of fixed lengths.** `(a)-[*1..2]->(b)` ≡ one-hop `UNION ALL` two-hop
   (with relationship-uniqueness handled). (GraphQE; PATHTest's whole thesis.)
4. **Pattern fusion.** `MATCH p1` + `MATCH p2` (as union) ≡ a single query whose pattern is the union
   of p1 and p2 (when they share no conflicting variables). Gamera's highest-yield MR (30/39 bugs).
5. **Pattern partitioning.** The inverse: a query over a union pattern ≡ union of queries over the
   parts. (Gamera.)
6. **Comma-separated ≡ chained patterns.** `MATCH (a),(b) WHERE a.x=b.y` ≡
   `MATCH (a) MATCH (b) WHERE a.x=b.y` (cartesian product associativity). (GraphQE "Cartesian
   product" axiom.)

**Predicate handling**
7. **WHERE relocation.** `MATCH … WHERE p RETURN …` ≡ `MATCH … WITH … WHERE p RETURN …` when no
   aggregation intervenes. (GraphQE redundant-clause rule; NoREC's predicate-relocation spirit.)
8. **Label predicate equivalence.** `MATCH (a:Label)` ≡ `MATCH (a) WHERE a:Label`. (Standard.)
9. **Predicate conjunction/disjunction partition.** `WHERE p` ≡ `WHERE p AND q` ∪ `WHERE p AND NOT q`
   ∪ `WHERE p AND q IS NULL` — the TLP core, specialised to a fixed pattern.
10. **EXISTS / pattern-predicate rewriting.** `WHERE EXISTS { (a)-->(b) }` ≡ an equivalent `MATCH`
    with a `count(*) > 0` guard (where the engine supports both forms).

**Projection / aggregation**
11. **WITH-rename transparency.** `MATCH (x) WITH x.name AS y RETURN y` ≡ `MATCH (x) RETURN x.name`
    (no intervening aggregation). (GraphQE.)
12. **DISTINCT idempotence.** `RETURN DISTINCT` applied twice ≡ once; `UNION ALL` preserves
    duplicates, `UNION` deduplicates. (GraphQE UNION axioms; standard.)
13. **Aggregation additivity.** `count()` over a partitioned row set equals the sum over partitions;
    `count(null) = 0`. (TLP's aggregate applicability; GDsmith found a `count(null)` mismatch bug.)
14. **Path-number product.** paths(A→C via B) = paths(A→B) × paths(B→C). (Gamera path-level MR.)
15. **Ancestor/descendant inverse.** `A ∈ ancestors(B)` ⟺ `B ∈ descendants(A)`. (Gamera.)
16. **k-hop symmetry.** `B ∈ outgoing-k-hop(A)` ⟺ `A ∈ incoming-k-hop(B)`. (Gamera.)

**Ordering / truncation**
17. **ORDER BY stability.** `ORDER BY k` must be a permutation of the unordered result (same
    multiset); ties may reorder, so compare as multisets. (Standard; Turso's unordered-LIMIT carve-out.)
18. **LIMIT/OFFSET windowing.** With a total order, `LIMIT n` ⊇ `LIMIT n OFFSET m`, and
    `(LIMIT n) ` concatenated with `(SKIP n LIMIT m)` ≡ `LIMIT n+m`. Without a total order, only
    multiset/size relations hold.
19. **SKIP monotonicity.** `SKIP m LIMIT n` has size `min(n, max(0, |R|-m))` for a total order.

**Optionality and nulls**
20. **OPTIONAL MATCH neutralization.** If the optional part always matches, `OPTIONAL MATCH` ≡
    `MATCH`; if it never matches, the result is the left side with NULLs for the optional variables.
    (GraphQE OPTIONAL MATCH axiom: `G(q1)×G(q2) + G(q1)×¬G(q2)×isNULL(G(q2))`.)
21. **NULL propagation.** Any expression with a NULL operand evaluates to NULL; `x IS NULL` and
    `x = NULL` differ. (TLP's third partition.)

**Writes / graph state**
22. **Graph-state persistence / isomorphism.** After a write, the queried subgraph must be
    isomorphic (LPG-iso) or subgraph-isomorphic (LPSG-iso) to the expected result. (GraspDB.)
23. **Write idempotence where specified.** `MERGE` run twice ≡ run once (for a fixed pattern);
    `CREATE` is not idempotent — both are testable relations.
24. **Irrelevant-data invariance.** Adding data that matches no pattern in `Q` must not change
    `RS(Q)`. (Gamera dynamic MR.)

**Soundness caution.** Before adopting any relation as an oracle, confirm it is genuinely
result-preserving under *bag* semantics and Cypher's relationship-uniqueness rule. GraphQE's
Table II is a machine-checked source for several of these; the Argus result (LLM-as-judge → 20/20
false positives) is the warning against validating an oracle by intuition.

---

## 6. Recommendations for Relate

### 6.1 Constraints and assets (grounded in the repo)

- **Harness:** `proptest` 1.11 with a working `ReferenceStateMachine`/model-graph harness already
  exists under `relate/relate-storage/tests/proptest_support/` (`GraphOp`, `ModelGraph`,
  `graph_op_sequence()`, `config()`). The query surface is **greenfield** — no query-level proptest
  yet.
- **Seed corpus:** the language **floor** is the openCypher TCK — 1,615 raw scenarios in
  `testdata/tck` at the time of this study (3,892 cucumber-expanded, all passing; one superseded
  outline removed from the pin per ADR-0188; allowlist of 3,892 exactly-passing ids) — and the
  **stretch** corpus was the CIP-derived (Cypher 25 / CIP) suite —
  **5,409 scenarios across 491 `.feature` files** in `testdata/cip-eval` (ratcheting scoreboard in
  `baselines/cip-eval.json`). The in-repo harness and both corpora left the engine on 2026-11-05
  (ADR-0520): the CIP-derived suite lives on at `features/cip-eval` in the spec harness, and the
  floor is the harness's synced `tck` suite (ADR-0482); the numbers above are the in-repo snapshot
  this study measured. Combined: a
  ~9.3k-scenario, already-valid, dialect-scoped pool —
  ideal as a mutation seed pool, and larger than any corpus in the studied tools' seed inputs
  (nothing in the literature uses a conformance suite this way; it is an advantage).
- **Reference engine:** a Neo4j server is reachable over Bolt with the
  `neo4rs` 0.8.0 crate (`AuraAuth`, `Graph`) — the client-side Bolt
  exchange is already exercised elsewhere in the tree. A local Neo4j
  container is a config change, not new code.
- **Plan introspection:** currently limited — `EXPLAIN` prints clause names only. Plan/operator
  coverage is **not measurable today**; exposing a physical plan tree is a prerequisite for anything
  QPG-like.

### 6.2 Build order (highest evidence-per-effort first)

1. **TLP-style query partitioning (primary oracle).** Adapt GDBMeter's graph partitioning to
   Relate. Generate `MATCH … WHERE p`, then `p`, `NOT p`, `p IS NULL` partitions and require the
   union to equal the original. Extend to aggregation (`count`), DISTINCT, and OPTIONAL MATCH.
   *Evidence:* GDBMeter found 14 logic bugs with **zero false alarms** and beat differential testing;
   TLP found 77 logic bugs in SQL. *Cost:* small — one generator plus three query rewrites, no
   reference engine. *Watch:* relationship-uniqueness and OPTIONAL NULL semantics (§1.1).
2. **Cypher metamorphic relations (§5, the reference-free subset).** Start with the highest-yield and
   cheapest: direction reversal, undirected ≡ union-of-directions, variable-length ≡ union of fixed
   lengths, pattern fusion/partitioning, WITH-rename transparency, OPTIONAL neutralization, and
   LIMIT/OFFSET windowing. *Evidence:* Gamera (pattern fusion alone found 30 bugs), GraphQE (proved
   equivalence), PATHTest (41 bugs). *Cost:* small; each MR is a query rewrite. This is the most
   Cypher-specific value for the least machinery.
3. **Grammar/TCK-seeded generation substrate.** A skeleton-grammar generator (GDsmith two-stage:
   skeleton + context typing) whose seed pool is the combined openCypher TCK (3,893 expanded) +
   CIP-eval (5,409 scenarios) corpus, with validity-preserving mutation
   (Squirrel-style IR mutation; GDsmith's DelayReturn/AdvanceReturn/RemoveCondition). *Evidence:*
   GDsmith's 100% semantic validity / 98% grammar coverage; Squirrel's validity-preserving mutations.
   *Cost:* medium, but it is the substrate oracles 1–2 and 4 all consume. If time is short, mutate
   the TCK `.feature` queries directly before writing a full grammar.
4. **Differential vs Neo4j (second pass, gated).** Reuse the existing `neo4rs` wiring. Run the
   generated corpus through both engines, but restrict to the shared dialect subset (the allowlist
   scenarios are a natural whitelist), skip undefined behavior, and carve out unordered LIMIT.
   *Evidence:* GDsmith (27 bugs) but also GDBMeter/GraphGenie (high false-alarm rates). *Cost:*
   medium; the cost is triage, not transport. Treat as additive, never primary.

### 6.3 What NOT to build first (and why)

- **PQS (pivoted query synthesis).** Most expensive oracle ("a single comparison operator alone
  covers already more than 200 LOC"); requires a typed expression generator that can invert a truth
  value. High value, but after 1–2.
- **Operator→query inversion (the user's idea).** No published precedent; requires a hand-maintained
  operator→fragment catalogue and, in Relate, a physical plan tree that does not yet exist. The
  supported version of the same instinct is QPG-style plan feedback on *forward* generation — and
  only once `EXPLAIN` exposes operators. Defer.
- **A from-scratch SQLsmith-style grammar fuzzer.** The TCK corpus already provides validity;
  spending the budget on a grammar duplicates it. Prefer mutation of TCK seeds.
- **DQP with forced plans.** Relate has no plan-hint facility; the practical variant is
  Kangaroo-style multi-plan *execution* (run all plans, compare), which is internal and hint-free.
- **A generic cross-engine differential fuzzer as the first oracle.** Highest false-alarm risk per
  §3; do it fourth, gated.

### 6.4 Effort / value summary

| Order | Component | Needs reference engine? | Needs plan tree? | Evidence strength | Relative cost |
|---|---|---|---|---|---|
| 1 | TLP / query partitioning | No | No | Very high (GDBMeter, TLP) | Low |
| 2 | Metamorphic relations (§5) | No | No | High (Gamera, GraphQE, PATHTest) | Low–medium |
| 3 | TCK-seeded generator + mutation | No | No | High (GDsmith, Squirrel) | Medium |
| 4 | Differential vs Neo4j | Yes (exists) | No | Medium (noisy) | Medium |
| 5 | Plan-coverage guidance (QPG) | No | **Yes** | High, but blocked | Medium–high |
| 6 | Operator→query inversion | No | **Yes** | **Thin/novel** | High |

### 6.5 Honest uncertainty

- Cypher-specific testing literature is **thin relative to SQL**: roughly a dozen tools (table in §2),
  all server-targeted, none embedded, none TCK-seeded. Relate would be **adapting** techniques,
  not reusing code.
- The claim "TLP is best for Cypher" rests mainly on **one** graph study (GDBMeter) plus the
  broader SQL evidence; no head-to-head study ranks TLP vs metamorphic vs differential on
  Cypher specifically.
- Operator-coverage testing for query languages is **under-studied**; ParSEval is about *data*
  generation and Kangaroo/DQP force plans at execution, not by synthesis. Treat any operator-driven
  design as research, not engineering, until plan introspection exists.

---

## Sources

- TLP — Rigger & Su, OOPSLA 2020: <https://www.manuelrigger.at/preprints/TLP.pdf> (DOI 10.1145/3428279)
- PQS — Rigger & Su, OSDI 2020: <https://www.usenix.org/system/files/osdi20-rigger.pdf>
- NoREC — Rigger & Su, ESEC/FSE 2020: <https://ar5iv.labs.arxiv.org/html/2007.08292> (DOI 10.1145/3368089.3409710)
- DQP — Ba & Rigger, SIGMOD 2024: <https://bajinsheng.github.io/assets/pdf/dqp_sigmod24.pdf> (DOI 10.1145/3654991)
- QPG — Rigger et al., ICSE 2023: <https://arxiv.org/html/2312.17510v1>
- SQLancer papers index: <https://github.com/sqlancer/sqlancer/blob/master/docs/PAPERS.md>
- SQLancer++ / Scaling Automated DB Testing: <https://arxiv.org/html/2503.21424v2>
- Argus (LLM oracle discovery): <https://arxiv.org/html/2510.06663v2>
- GDsmith — Hua et al., ISSTA 2023: <https://taoxiease.github.io/publications/issta23-gdsmith.pdf>, <https://arxiv.org/html/2206.08530>
- GDBMeter (Query Partitioning for graphs) — Kamm et al., ISSTA 2023: <https://matteokamm.ch/issta2023_paper.pdf> (DOI 10.1145/3597926.3598044)
- Gamera — Zhuang et al., VLDB 17(4):836, 2024: <https://www.vldb.org/pvldb/vol17/p836-zhuang.pdf>
- GraphGenie — Jiang et al., ICSE 2024: <https://yuanchengjiang.github.io/docs/GraphGenie-ICSE24.pdf>
- GRev — Mang et al., ICSE 2024: <https://joyemang33.github.io/assets/pdf/GRev.pdf>
- GraspDB — Liu et al., ISSTA 2024: <https://xgdsmileboy.github.io/files/paper/issta24-db.pdf> (DOI 10.1145/3650212.3680311)
- QuDi — Zheng et al., ISSTA 2024: <https://2024.issta.org/details/issta-2024-papers/135/> (DOI 10.1145/3650212.3680392)
- Dinkel — Jiang et al., VLDB 2026 / arXiv 2408.07525: <https://arxiv.org/html/2408.07525v2>
- PATHTest — Gui et al., VLDB 19(9):2234, 2026: <https://www.vldb.org/pvldb/vol19/p2234-gui.pdf>
- GraphQE — ICDE 2025: <https://ar5iv.labs.arxiv.org/html/2504.15742>
- GDSynth — ICSE 2026 demo: <https://conf.researchr.org/details/icse-2026/icse-2026-demonstrations/8/>
- SQUIRREL — Zhong et al., CCS 2020: <https://changochen.github.io/publication/squirrel_ccs2020.pdf>
- SQLsmith: <https://github.com/anse1/sqlsmith>
- ParSEval — Chen et al., VLDB 18(11):4750, 2025: <https://www.vldb.org/pvldb/vol18/p4750-miao.pdf>
- Kangaroo (multi-plan execution) — Li et al., Computers & Security 157:104564, 2025: <https://yajin.org/papers/cose2025_dbms.pdf>
- DQE — Song et al., ICSE 2023: <https://wsdou.github.io/papers/2023-icse-dqe.pdf>
- ACME — Jiang et al., NUS: <https://yuanchengjiang.github.io/docs/acme.pdf>
- DBcover — ICSE 2026: <https://arxiv.org/abs/2608.25573>
- Turso differential fuzzer: <https://github.com/tursodatabase/turso/blob/main/.claude/skills/differential-fuzzer/SKILL.md>
- APOLLO: <https://db.cc.gatech.edu/apollo-automated-testing-of-database-systems/>
- Thanos — ICSE 2025: DOI 10.1109/ICSE55347.2025.00001
- Rigger DBMS bug list: <https://www.manuelrigger.at/dbms-bugs/>
