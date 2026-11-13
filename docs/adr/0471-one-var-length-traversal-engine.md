# ADR-0471: One traversal engine for every var-length spelling

Status: Proposed

Claim: a7f0aa02311c9b2500f45bfede6e9a09d4e03d08

## Context

A variable-length relationship can be spelled four ways: the legacy
bracket form `-[r:T*m..n]-`, the postfix quantifier
`-[r:T]->{m,n}`, the parenthesized quantified group
`(()-[r:T]-()){m,n}`, and any of those under a path-mode prefix
(`p = TRAIL …`, `p = WALK …`). Until this decision they lowered onto two
engines: the classic compact walker (`exec/expand.rs`) for the legacy
bracket form, and the GPM engine (`exec/gpm/`) for the group form, the
mode-prefixed forms, and `REPEATABLE ELEMENTS`.

The PathDB spike and the QPP speed-of-light measurements (research notes
`pathdb-spike-results-2026-11-01.md`, `qpp-speed-of-light-2026-11-01.md`,
LABS-661) measured a ~1,000x spelling-dependent cliff on GBL SF0.1 —
identical language, identical 1,327-path answer:

| Spelling | Engine path | Start selection | Time |
| --- | --- | --- | --- |
| `-[r:knows*1..2]-` | classic walker | WHERE `x.ID = …` → index seek, 1 node read | 4.3–4.9 ms |
| `((a)-[:knows]-(b)){1,2}` | GPM quantified DFS | label scan, 1,528 node reads | 5,176 ms |
| `p = TRAIL …-[r:knows*1..2]-` | GPM rel DFS | label scan, 1,528 node reads | 3,570 ms |

The cliff had two parts. The start-point selection in `match_gpm_path`
ran with an empty residual, so a seek-able WHERE conjunct on the start
variable never reached `node_candidates` — every Person was a start and
each paid the full quantified expansion before the post-filter dropped
it. And per start, the generic machinery re-derived what the classic
walker had specialized. Same answer, spelling-dependent cost: a user
trap that no planner hint can excuse.

## Decision

1. **Lower every var-length spelling onto the one quantified IR.** At
   lowering (`cypherlite-front`), each top-level var-length relationship
   element is replaced — position-preserving — by the `Quantified`
   element the parenthesized group produces: implicit anonymous boundary
   nodes inside the group, the rel clamped to a plain one-hop (its
   `*m..n` bounds move onto the quantifier; its direction, types,
   properties, variable, and inline `WHERE` stay), no subpath variable,
   no group predicate. The rel's neighboring node elements stay in
   place, so a chain `(a)-[:R*1..2]->(b:B)-[:S*1..2]->(c)` quantifies to
   `(a)((_)-[:R]->(_)){1,2}(b:B)((_)-[:S]->(_)){1,2}(c)` — the interior
   node is the first group's target and the second group's start,
   matched once, exactly the legacy semantics. `needs_gpm` is therefore
   true for every spelling and the GPM quantified traversal is the one
   engine. The compact `nodes`/`rels` projection is retained, computed
   from the raw alternating sequence: the eval-pattern engine (pattern
   predicates, pattern comprehensions, `shortestPath()` expressions)
   walks that view, and its var-length behavior is frozen TCK surface.

2. **The start-point index seek lives in the QPP traversal.**
   `gpm_start_nodes` now receives the MATCH-level WHERE residual, the
   same residual the classic `match_node` consults: `node_candidates`
   turns an equality/CONTAINS conjunct on the start variable into an
   index seek (NodeReads=1), ready conjuncts filter the starts, and
   conjuncts naming not-yet-bound variables stay with the caller's
   post-filter. The walk itself keeps its empty residual — inline
   element predicates per element, the WHERE afterwards.

3. **Unbounded depth parity.** The quantified DFS's unbounded
   (`{m,}` / legacy `*`) depth cap is the relationship count under the
   default DIFFERENT RELATIONSHIPS mode — the same sound bound
   `gpm_expand_relationship` and the classic walker use — so every
   spelling of an unbounded var-length explores to the same depth.
   REPEATABLE ELEMENTS never terminates by uniqueness and keeps its
   satiety cap (ADR-0466's admission limit still governs explicit
   bounds).

4. **Var-length rel variables keep the legacy reuse rules; group
   variables keep the quantified-path rules.** The group-name collection
   distinguishes the two origins: the rel variable of a quantified var-length
   relationship may repeat across patterns and may be pre-bound to a
   list (`WITH [r1, r2] AS rs MATCH (a)-[rs*]->(b)` pins the exact
   trail — the QPP accept now checks the accumulated list against that
   binding, the compatibility check `gpm_rel_dfs` always applied);
   a parenthesized group's variables admit no reuse and no pre-binding.
   The one shape decoder — `QuantifiedPathPattern::implicit_var_length_rel`
   in `cypherlite-ir` — serves the shortest-route guard, the plan-text
   renderer (every spelling renders in the compact form, so EXPLAIN
   shows one plan text per pattern), and the origin rules.

5. **The shortest route keeps its spelling-scoped admission.** The
   route (ADR-0186) decodes the quantified shape for the legacy
   bracket form; the postfix spelling keeps its conservative GPM path —
   its ANY-tie choice is pinned there by the selective-path tests, and
   the route's tie order differs.

Measured after the change (GBL SF0.1, degree-median Person seed, same
store and seeds as the spike; medians over warm repeats, release):

| Query | Before | After |
| --- | --- | --- |
| `knows*1..2` DISTINCT y (compact) | 4.3–5.7 ms | 4.4 ms |
| `((a)-[:knows]-(b)){1,2}` DISTINCT y (group) | 5,176 ms | 5.7 ms |
| both spellings, `count(*)` over the match | — | 2.9 vs 4.0 ms |
| both spellings, all 1,327 paths listed | — | 5.8 vs 7.1 ms, identical rows |
| `p = TRAIL …knows*1..2…` PROFILE | 1,528 db hits (label scan), 3,570 ms | 1,328 db hits (seek + 1,327 hops), 3.6 ms |

Answers are identical across spellings (1,327 paths, endpoint sets
equal); the trail-vs-WALK divergence (the seed's rel-reusing 2-walk)
is unchanged and documented in the spike note.

## Alternatives considered

- **Push the start-point seek into the generic machinery without
  unifying the IR.** The seek alone removes most of the cliff, but two
  engines remain: every traversal fix must be made twice, and the
  spelling-dependent semantics risk (two walkers that must agree on
  trail, used-set, and binding behavior) stays. Rejected: the directive
  is one semantics implementation, not one faster engine beside a slow
  one.
- **Rewrite the group form to the compact form** (normalize quantified
  groups onto var-length rel elements, keep the classic walker).
  Rejected: the direction loses. The QPP engine is where the mid-path
  `allReduce` inlining (this feature's `allreduce-inlining` node) and
  the frontier-kernel work land, and a parenthesized group's named
  inner elements, per-iteration predicates, and subpath variables have
  no compact representation — the group form is the strictly more
  expressive IR, so the less expressive one lowers onto it.
- **Leave both engines and document the cliff.** Rejected: a ~1,000x
  spelling-dependent performance difference is a user trap; the
  faithful PathDB spelling and `REPEATABLE ELEMENTS` both fall off it
  without any signal in the query text that spelling choice is a
  performance decision.
- **Also clear the compact `nodes`/`rels` view for full single-struct
  IR identity.** Deferred, not rejected: the eval-pattern engine and
  the pattern-predicate lowering read the compact view (`WHERE
  (a)-[*1..2]->(b)`, pattern comprehensions, `shortestPath()`
  expressions — frozen TCK behavior); rerouting that engine onto the
  GPM matcher is its own change. The engine-relevant IR (`elements`)
  is identical across spellings today; the retained view is the
  eval-pattern engine's input, stated here so the two views cannot
  silently drift apart.

## Consequences

- Every spelling of a var-length pattern takes the GPM quantified
  traversal with one plan shape and one EXPLAIN subject text; the
  measured parity is 1.2–1.4x between spellings where it was ~1,000x.
- Fast paths that gate on `needs_gpm` now decline var-length patterns:
  the EXISTS-subquery probe and the one-hop aggregate folds (both
  already declined `var_length`) fall back to the generic matcher.
  Correctness is unchanged; the exists probe loses its early-exit for
  var-length shapes — a follow-up candidate beside the frontier-kernel
  dispatch ticket.
- A pre-bound rel list (`WITH [r1,r2] AS rs MATCH …-[rs*]->…`) is
  answered by enumeration-with-compatibility-check rather than the old
  seek along the pinned rels: same rows, more work on that niche
  feature.
- The postfix spelling's `quantified` marker survives on the inner rel
  (its per-edge inline-WHERE post-filter semantics) and keeps it off
  the shortest route; a marker-less postfix form is indistinguishable
  from the legacy form everywhere else.
- The classic walker remains the engine for plain fixed-length compact
  patterns (no quantified element) — the unification is scoped to
  var-length traversal, where the spelling split lived.

## Later references

ADR-0475 restores the early exit this decision's Consequences record
losing: GPM patterns (every var-length spelling) probe through the GPM
walk with an EXISTS probe rider that halts at the first accepted row,
the same walk now also evaluating qualifying MATCH-WHERE conjuncts at
its binding points and halting on a walk-riding LIMIT cap. The fast
paths still decline var-length patterns into the generic route — the
rider is inside the GPM engine, not a new fast path beside it.

ADR-0478 gives unbounded cyclic recursions a kernel path beside this
engine: the frontier-NFA QPP kernel answers the frontier-sound shapes
(reachability/EXISTS, distinct endpoints, un-ordered capped paths, bounded
WALK) directly. The DFS remains the engine for every shape the frontier's
answer-shape gate declines — fail-through, answers identical.
