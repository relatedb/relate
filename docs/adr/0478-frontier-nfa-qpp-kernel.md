# ADR-0478: The frontier-NFA QPP kernel

Status: Proposed

Claim: 50180de65cdaeec90c5c4613b6e3c057519b7ef8

## Context

The PathDB spike (research notes `pathdb-spike-results-2026-11-01.md`,
`qpp-speed-of-light-2026-11-01.md`, LABS-661) measured what the unified
var-length engine cannot fix: on GBL SF0.1, 42 of 142 PathDB queries time
out under the suite's 10 s watchdog with 37–43 GB peak RSS — every one a
recursion over the cyclic Person-centric types (`knows`/`likes`/
`hasCreator`). The default pool enumerates TRAILs, and trail counts on a
cyclic graph are exponential in the diameter while the answers are tiny:
`RETURN p LIMIT 100`, a distinct-endpoint aggregate, or an EXISTS verdict.
The per-branch clones of the DFS cursor (`state.row`, `groups`, `used`
per iteration, `exec/gpm/qpp.rs`) are what the RSS numbers measure.

ADR-0471 made the GPM quantified DFS the one engine for every var-length
spelling — one semantics, one plan — but it is a per-row *trail* DFS: it
enumerates every path. ADR-0473 added the contracted-kernel layer
(structural matcher + cost gate; everything it cannot prove declines to
the unchanged walker, answers identical) and charted the frontier-NFA
class as this follow-up. ADR-0475's riders gave that walk an early exit
(EXISTS) and a walk-halting LIMIT cap — but only *after* the same
exponential enumeration has run.

The research prototype quarantined behind ADR-0476's non-default
`research` feature (`cypherlite-executor/src/graph/qpp.rs`) measured both
the gap and the semantics: 45x–37,342x between the DFS and a
level-synchronous BFS over deduplicated (node, automaton-state) pairs,
and exactly one divergence class between the frontier's walk-reachability
answers and the engine's trail answers — measured and pinned, not
guessed. This decision productionizes it.

## Decision

Add the **frontier family** to the contracted-kernel layer (ADR-0473):

1. **The kernel.** A Thompson NFA compiled at plan time from the
   `QuantifiedPathPattern` IR element — the one element every var-length
   spelling lowers to since ADR-0471, so every spelling gets the kernel —
   over interned rel-type ids with direction-tagged transitions and hop
   bounds `{min, max: Option<u32>}`. The run is a level-synchronous BFS
   over deduplicated (node, automaton-state) pairs on the `ExecGraph`
   overlay view — write scratch and pinned ts visible, the same liveness
   rules `hop` applies, the cancel window sampled per level — not the
   base `Graph` the prototype reads. **Accept on arrival, before the
   dedup filter**: an accepting transition reaching `v` at a hop count in
   `[min, max]` marks the endpoint even when `v` was first visited at an
   earlier level; dedup gates *expansion* only. Parent-pointer plus
   traversed-rel-id tables (one `RelId` per (node, state) slot) feed an
   output-sensitive capped reconstruction — cap × depth after the BFS,
   never per trail — binding the row shapes the DFS would emit: the path
   variable, the group lists, the target's bindings. Structural proofs
   decline (nested quantifiers, state count above the 64-pair bitmask
   ceiling, uncompilable outer context); the cost gate engages on the
   planner's own estimates — an unbounded quantifier over any type, or
   estimated paths at a constant factor (4x in the first cut) over
   estimated distinct endpoints; bounded acyclic-schema recursions
   (replyOf/isSubclassOf forests, measured 0.2–0.7 ms) stay on the DFS.

2. **The answer-shape gate** — the semantic half, as much a proof as the
   structural half. The frontier serves exactly the answer shapes that
   are functions of the (node, state) *pair*, not of the *path*:
   reachability/EXISTS (an exists-probe arm), distinct-endpoint answers
   (`count(DISTINCT y)`, `RETURN DISTINCT y`, arrival-time folds over
   distinct endpoints such as `min(y.k)`), un-ordered capped path
   emission (`RETURN p LIMIT n`), and bounded WALK mode. Everything else
   declines to the unchanged DFS — fail-through, answers identical,
   pinned by differential tests: full enumeration (uncapped
   TRAIL/SIMPLE/ACYCLIC — two trails to one node are two answers),
   ordered caps (ORDER BY + LIMIT needs the full candidate set to sort),
   selectors (ANY/ALL SHORTEST — the GPM shortest machinery owns them;
   they already stand down early conjuncts and the inliner), path counts
   (`count(p)` counts paths, not endpoints), patterns whose quantification
   accumulates a group any inlined allReduce (ADR-0469) reads — the
   inliner's per-branch prefix semantics cannot survive dedup: one
   (node, state) pair stands for many prefixes with different
   accumulator values, so that shape keeps the DFS with its inliner —
   and rel-predicate hops (an inner rel's `WHERE` declines; an inner
   node's in-vocabulary `WHERE` rides as an arrival-time check).

3. **The re-arrival trail-existence rule** — default-pool exactness
   (DIFFERENT RELATIONSHIPS ⇒ TRAIL pool). The prototype's measured
   divergence: on SF0.1 `knows{1,2}` the engine's trail answers count
   855 distinct endpoints, the frontier 856 — the delta is the seed,
   re-reached by a rel-reusing 2-walk whose shortest *trail* falls below
   `min`; the engine's own true-walk spelling returns exactly 856. A
   re-arrival that reaches an already-visited (node, state) pair is
   accepted only when a trail to that endpoint within `[min, max]`
   exists — a bounded local check per re-arrival, never per path —
   pinned by differential tests on parallel-rel and self-loop graphs
   (GBL has neither; the TCK graphs have both). The differential corpus
   measured two generalizations of that rule, both landed with it: the
   note's "first arrivals never diverge (a shortest walk is always a
   trail)" claim fails for context+round automata sharing a type — a
   rel-reusing walk can *first*-arrive a fresh pair, measured: the
   engine answers one endpoint where the unruled frontier answered
   three — so every accepting arrival whose representative
   parent-chain walk is not node-simple (node-simple ⇒ rel-distinct,
   the cheap proof that covers the flat family's first arrivals) goes
   to the bounded search; and a node whose every *observed* accepting
   arrival sits below `min` can still hold an in-bounds trail whose
   final hop departs a pair the dedup filter expanded only at its own,
   earlier level (a self-loop at the seed) — such arrivals are flagged
   as candidates and the rule answers once per candidate after the
   BFS. The remedy is the bounded trail-existence search (the note
   §7.1's (a)), not the cheaper cycle-closing-trail formulation (b):
   (b) is type-blind — a same-pair rel of a type the transition cannot
   traverse repairs nothing, a wrong admission where (a) proves.
   In-feature amendment (`walk-pool-min-gap`): the below-min candidate
   rule is pool-universal — the WALK pool shares the dedup blind spot,
   because walk semantics have a `min` too, and a self-loop at the seed
   hides the 2-walk `1-(loop)-1-(rel)-0` of `*2..3` exactly as it hides
   the trail (engine REPEATABLE `{0,1}`, walk pool `{1}` before the
   amendment, measured). A walk-pool candidate is admitted by the
   **walk-length check** — does an in-bounds WALK exist, rels reusable —
   a reachability-within-hop-bounds search over the same NFA whose
   position `(node, state, hops)` is the whole search state: no rel set
   to carry, so failed positions memoize and the search is polynomial
   in pairs × hop bound, strictly cheaper than the trail search. The
   completeness argument carries over from the default pool: any
   in-bounds endpoint's final hop departs a pair the BFS expanded at
   its first-arrival level, so the same accepting transition fires at
   an observed hop — in-bounds (admitted outright, the pool's native
   semantics) or below-min (candidate) — and the check admits nothing
   without a found walk: exact in both directions, every production
   walk-pool shape being bounded (Decision 5 keeps unbounded REPEATABLE
   rejected), pinned by the per-pool differential (the walk pool
   against the engine's REPEATABLE ELEMENTS spelling).

4. **Row order for capped paths.** LIMIT without ORDER BY has no order
   contract. ADR-0475's cap rider promises "the rows the materializing
   walk would emit, in the same order — only sooner" because it halts
   the engine's own enumeration; that promise does not extend to
   frontier-served capped emission: BFS-discovery order replaces DFS
   order, and the *set* of first-cap rows can differ from the DFS's
   (both valid, both contract-free prefixes). Per-level reconstruction
   approximates the DFS's depth order but is an implementation choice,
   not a promise.

5. **The unbounded-WALK rejection is unchanged in v1.** Unbounded
   REPEATABLE ELEMENTS keeps its authored `UnsafeRepeatableElements`
   rejection (Neo4j-2026.05-aligned; ADR-0471 §3's satiety cap still
   governs explicit bounds). The frontier could answer unbounded WALK
   reachability and capped paths soundly in milliseconds — spec fidelity
   wins: the 42 timeouts are all default-pool shapes. Relaxing the
   rejection is a behavior change visible to users and is noted here as
   a possible future ADR, not taken.

6. **The work budget, and the allocation bound that composes with
   it.** A fixed-constant edge-visit budget in the ADR-0395 family — no
   ambient knob — with an authored typed error
   (`WalkBudgetExceeded`), checked per level: a frontier that would blow
   up (huge diameter × states) fails loud in milliseconds instead of by
   watchdog cancel after RSS growth. Kernel-served queries also tick the
   nested-evaluation budget once per evaluation, exactly like the kernel
   arm in `exists_probe.rs` (one evaluation = one tick, never on
   decline). Declined shapes keep the DFS and its separate walk-step
   budget ticket — the kernel does not rescue them. The allocation
   half (the availability fix on the draft): the dense `span × states`
   tables — the run's parent/dedup set plus the re-arrival rule's
   span-indexed memo and stamps — are allocated **once per engaged
   clause** and recycled across its seeds, the per-seed reset walking
   the run's first-arrival pair ledger (O(pairs touched), never a
   span-wide sweep) and the rule's memo carrying a per-run generation
   stamp; a clause whose tables would exceed a fixed-constant byte
   ceiling declines before allocating — the DFS fall-through, chosen
   over the authored error deliberately: a span the tables cannot fit
   is a store-shape fact (a huge or id-churned store), not a runaway
   walk, and fail-through is the family's direction. The two bounds
   compose: the budget bounds each run's edge visits, the ceiling the
   clause's one table set.

7. **De-quarantine.** ADR-0476's research module is replaced by (unified
   with) the production kernel: the `research`-gated `graph/qpp.rs` and
   its harness are deleted outright (the constructor-derived expected
   shapes stay pinned as literals in `exec/kernels/nfa_tests.rs`, and
   the differential suite plus the PathDB/LDBC QPP corpus are the
   behavioral oracles — no live parity module remains), and the
   prototype's file-local `with_node` copy disappears with the module,
   leaving sol.rs's file-private helper the one canonical spelling.

## Alternatives considered

- **Optimize the DFS instead** (streaming LIMIT emission, index-seek
  pushdown, memory tuning of the cursor clones). Rejected: the
  enumeration itself is exponential in the cyclic case — every such fix
  keeps per-path work; those improvements remain separate, complementary
  tickets for the DFS-served shapes, not a substitute for collapsing the
  state space.
- **Dijkstra/shortest-path machinery for the reachability shapes.**
  Rejected: shortest-path semantics are a different contract (the
  selector family, declined here anyway) and offer no hop-bounded
  alternation support — `(A.B)+`, `(A|B){m,n}` have no
  single-relaxation form.
- **Always-frontier — dedup every answer shape and return walk
  semantics throughout.** Rejected: unsound for TRAIL/SIMPLE
  enumeration — distinct paths are distinct answers; this changes
  answers, not cost. The measured 855-vs-856 divergence is the smallest
  instance of exactly the class this alternative would generalize.
- **Per-level bitsets instead of the ever-visited set.** Rejected:
  accept-on-arrival re-reach semantics need the ever-visited (node,
  state) set; per-level dedup changes answers (a pair re-reached at a
  later in-bounds level must still count as an endpoint).

## Consequences

- The 42 timed-out PathDB SF0.1 shapes (and SF1's 13) answer at
  millisecond scale with the frontier engaged (prototype measurements:
  sub-ms to 18 ms); they land as acceptance cases in the
  `cypherlite-query-latency` harness.
- Memory for kernel-served shapes is O(nodes × states), not O(paths):
  the 37–43 GB RSS class disappears for every shape the gate admits.
- Kernel-served caps halt early: the BFS stops once `cap` witnesses are
  reconstructed and accepted; distinct-endpoint answers run to fixpoint
  or `max` — bounded by the pair set, never the path count.
- A new decline surface to maintain: every answer-shape and structural
  gate needs a decline-suite test, and the differential suites
  (frontier vs test-hook-disabled DFS on cyclic graphs — parallel rels,
  self-loops, mixed alternations, 2-state round groups, `min > 1`
  bounds, re-arrival shapes, directed/undirected variants) pin answer
  identity; the TCK stays 3892/3892.
- Capped path emission gains a different row order (Decision 4) —
  documented here and unobservable to every order-insensitive consumer
  (the TCK's any-order steps, the latency suite's answer checks); a
  pinned test assuming DFS order on an engaged shape would surface
  immediately in CI.
- Unbounded REPEATABLE ELEMENTS still rejects; relaxing it for
  kernel-served answer shapes is a possible future ADR (Decision 5).
- Full enumeration, path counts, and selectors keep the DFS and its
  separate walk-step budget ticket; the allReduce inliner keeps its
  shapes.
- Engine-internal (`cypherlite-executor`, the `exec/kernels/` and
  `exec/gpm/` families): no public API, protocol, client surface, or
  dependency moves. ADR-0476's `research` feature loses its `qpp` half
  (module, re-exports, harness example — all deleted); the feature
  itself survives for the `sol` half, which stays quarantined until the
  LABS-455 SPI.

## Later references

- ADR-0516 supersedes Decision 3's budget corner: a work-budget-cut
  re-arrival search is no longer a conservative rejection but a
  tri-state `Exhausted` verdict, and the run carrying one declines to
  the DFS (`FrontierDecline::RearrivalBudget`) instead of shipping an
  unproven endpoint set (LABS-952).
- ADR-0523 extends Decision 2's answer-shape vocabulary with a walk-count
  consumer (a plain `count` under the bounded WALK pool with an unfiltered
  target, answered from per-level pair multiplicities), the one path count
  the family serves; the same change corrects Decision 1's cost-gate
  estimates (later hops priced over the path multiset, the
  distinct-endpoint estimate bounded by the store's node population).
- ADR-0521 supersedes Decision 6's allocation half in its layout, reset
  and gate: the frontier tables are sparse (sized by the pairs a run
  touches, never by the store's node-id span), reset per seed in O(1) by
  a generation stamp, and held to the table ceiling by exact accounting
  at every level boundary — a crossing is a run-time decline to the DFS
  — instead of a `span × states` estimate before allocation.
- ADR-0582 extends Decision 2's boundary-predicate vocabulary with `<>`
  between the two boundary variables for the distinct-endpoint consumer,
  and makes the plan-time gates' declines visible on the PROFILE record.
- ADR-0536 generalizes Decision 1's input space beyond the var-length
  spellings: one regular pattern over the closed atom vocabulary with
  at most one range-quantified region — fixed-length chains as the
  zero-region case, exact-count regions unrolled to per-level states.
