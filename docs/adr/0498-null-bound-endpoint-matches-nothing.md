# ADR-0498: A null-bound pattern endpoint matches nothing in every engine

Status: Proposed
Claim: fd60483b5813441e8143649a31797f510de8b593

## Context

The walk vocabulary's binding classification
(`cypherlite-executor/src/pattern_walk/mod.rs`) states the family rule in
one line: a variable bound to a null or wrong-typed value is a
`Binding::Mismatch` and *matches nothing*. LABS-711 verified the rule
against a live Neo4j 5 for the single-hop shape and pinned it
(`a_null_prebound_rel_variable_is_not_rebound_by_the_classic_walker`,
`exec/gpm/gpm_tests.rs`, classic-walker arm); PR #644 (4274a3bdd)
aligned the shortest-expression fast path to it
(`Binding::Mismatch => no trail`, pinned in
`exec/shortest_expression_tests.rs`); the candidate funnel short-circuits
a `Mismatch` to zero candidates (`pattern_walk/candidates/mod.rs`,
`binding_arm`). But the rule has never been *ruled* for the endpoint of a
var-length pattern, and since ADR-0471 lowered every var-length spelling
onto the one quantified IR — `needs_gpm` is true for every spelling, the
GPM engine is the one var-length engine; only the eval walk (pattern
comprehensions, pattern predicates, `shortestPath()`/`allShortestPaths()`
expressions) walks the frozen-TCK compact view — that question covers
several user-visible spellings reaching two engines (LABS-863; the
divergence was found by LABS-811's investigation).

Probes run for this ADR against origin/main (72dc4642e), graph
`(:N {id:'a'})-[:R]->(:N {id:'m'})-[:R]->(:N {id:'b'})`, the endpoint
pre-bound to null via `WITH a, null AS b` — the failed-`OPTIONAL MATCH`
spelling behaves identically; positive controls (a real node binding)
answer on every row:

| Spelling | Engine today | CypherLite | Neo4j 5.26 | Neo4j 2025.10 |
| --- | --- | --- | --- | --- |
| `[(a)-[*]->(b) \| 1]` comprehension | eval walk | `[]` | `[]` | `[]` |
| `shortestPath((a)-[*]->(b))` | eval shortest | `null`, no search | `NULL` | `NULL` |
| single-hop `MATCH (a)-[r:R]->(b)` | classic walker / GPM single-hop | 0 rows | 0 rows | 0 rows |
| `MATCH p = (a)-[*]->(b)` (var-length) | the one var-length engine (GPM, ADR-0471) | **2 rows** (len 1, 2) | **0 rows** | **0 rows** |
| `MATCH (a)-[:R]->+(b)` (quantified) | GPM | **2 rows** (m, b) | n/a | **0 rows** |
| `MATCH ANY SHORTEST (p = (a)-[*]->(b))` | route guard declines the null end, GPM walker answers | **2 rows** (len 1, 2) | n/a | **0 rows** |
| `MATCH (x)-[t*1..2]->(c)`, `t` a pre-bound trail, `c` null | GPM exact-trail pin | **1 row** (`b`) | — | — |
| `MATCH p = (s)-[*]->(b)`, null-bound **start** `s` | same engines | 0 rows | 0 rows | 0 rows |

The wrong-typed endpoint (`WITH 1 AS b`) is already out of scope: a
pattern position reusing a scalar-bound name fails at plan time
(`VariableTypeConflict`). Null is typeless and passes that check — which
is exactly why it is the open case.

The mechanism, verified against the tree: the GPM engine's rebinding
policy `prebound()` (`exec/gpm/mod.rs`) reads *absent or null* as
`Prebound::Fresh` — the variable rebinds — and `gpm_bind_node`'s Fresh
arm feeds every node rebinding site, including the walk's end/target
accepts. The start side never diverges because start resolution flows
through the candidate funnel (`gpm_start_nodes` → `node_candidates`),
whose `binding_arm` answers a `Mismatch` with zero candidates; the
engine is asymmetric on the null today — start matches nothing, end
rebinds. The policy's doc cites ADR-0335 for null-is-fresh, but
ADR-0335's Decision names only two null-binding sites, both
machinery-seeded: the non-local singleton (a predicate-referenced
name that neither the entering row nor the quantification binds reads as
null — `gpm/qpp.rs` seeds it on the walk's base row) and the void
`OPTIONAL CALL` unit row. `prebound()` generalized null ⇒ fresh to every
rebinding site, including nulls the *entering row* carries — a failed
`OPTIONAL MATCH`'s variables, a `WITH null AS b` — which ADR-0335 never
ruled. The rel side of that generalization is pinned as a visible
divergence (the gpm_tests pin's GPM arm) and explicitly left for an ADR;
the end side was never pinned at all — no test asserts the divergent
rows.

A second, latent spelling of the same collapse sits in the shortest
route: `Route::bound` (`exec/gpm/shortest_route/route.rs`) reads the end
through `bound_node`, which collapses `Mismatch` into the unbound arm
(`binding(...).bound_id()`), so a null-bound end would anchor
`Anchor::Start` and search for any matching end. Today that collapse is
unreachable — `route_shape`'s `endpoint_ok`
(`exec/gpm/shortest_route/guard.rs`) declines an endpoint variable bound
to anything but a node before the route is built, and the GPM walker
answers instead — but it is the route's own spelling of null-is-fresh
and must move with whatever ruling wins.

## Decision

**Recommended: null-matches-nothing, tree-wide, for a pattern endpoint
the entering row binds to null** (option (a) of LABS-863's framing). A
pattern element's variable that the row entering the pattern carries as
null — from a failed `OPTIONAL MATCH`, a `WITH null AS …`, any
null-producing expression — is a binding to compare against and never
matches, in every engine and every spelling: single-hop, var-length
(`[*]`), quantified (`-->+`), `ANY`/`ALL SHORTEST` selectors, and
pre-bound-trail (`(x)-[t*]->(c)`) shapes. Both endpoints: the start side
already conforms everywhere (funnel short-circuit); the ruling closes the
end side. The recommendation's grounding, in order of weight:

1. **Neo4j ground truth, both dialect generations, on the very shapes in
   question.** Neo4j 5.26 and 2025.10 both return zero rows for the
   var-length `MATCH p = (a)-[*]->(b)` with a null-bound end, and
   2025.10 — the GPM-era dialect — returns zero rows for the quantified
   and `ANY SHORTEST` spellings too. CypherLite's two rows are not a
   dialect position; they diverge from every external anchor.
2. **The family's own rule, already pinned on three sides.** LABS-711's
   Neo4j-verified single-hop pin, the funnel's `Mismatch` short-circuit,
   and #644's shortest-expression alignment all say null-matches-nothing;
   the eval walk — the frozen-TCK reference surface — already answers
   `[]`/`null` on the same graph.
3. **The divergence has no origin of its own.** It is `prebound()`'s
   generalization of ADR-0335's null arm beyond the two machinery-seeded
   sites that arm names; nothing in ADR-0335's text or in Neo4j's
   behavior ever asked an entering-row null to rebind.

The carve-out, stated so the implementation cannot break it:
**ADR-0335's two null sites keep fresh semantics** — the
non-local singleton's machinery-seeded null and the void `OPTIONAL CALL`
unit row. The ruling splits on the null's *source*: machinery-seeded
nulls rebind (they are the pattern's own forward references,
Neo4j-verified); entering-row nulls match nothing. The
seeding site's own contract — qpp.rs seeds null only for names the
entering row does not bind and the quantification will not — makes the
distinction decidable at the reconciliation sites (entering-row snapshot
or marked seedings, the implementer's choice).

The shortest route moves with the ruling: `Route::bound` reads the full
classification — a `Mismatch` end is no partition, never
`Anchor::Start` — so the latent collapse cannot resurface if the guard's
endpoint check ever relaxes; `route_shape`'s decline of null endpoints
stays as-is (harmless once the walker answers zero rows).

**This ADR is Proposed — the human decides.** Acceptance turns the
follow-up work below from conditional to immediate.

## Alternatives considered

- **Codify the divergences as deliberate (option (b)): pin each engine's
  current behavior and document why.** Rejected as the recommendation:
  there is nothing to codify against — Neo4j 5.26 and 2025.10 both
  answer zero rows on the exact spellings that diverge, the family rule
  and the pinned eval-walk reference point the other way, and the
  behavior's only origin is an over-generalized null arm. Pinning it
  would make the divergence permanent and load-bearing: every future
  engine change would have to preserve an answer Neo4j never gives.
  Deferral note: if the human wants GPM's null-is-fresh kept as a
  CypherLite-specific semantic, this option is the vehicle — pins per
  spelling (including the route-anchor spelling), plus a `prebound()`
  doc comment stating the deliberate divergence from Neo4j — but it
  should then rule the rel side in the same breath, since one policy
  would be serving both.
- **Align the eval walk up to null-is-fresh instead (one semantics, the
  other direction).** Rejected: it contradicts Neo4j on every probed
  spelling, breaks the pinned #644 test and LABS-711's pin, and
  unfreezes the TCK comprehension surface the eval walk exists to hold.
- **Fail closed with an error on a null-bound endpoint**, mirroring the
  wrong-typed endpoint's plan-time `VariableTypeConflict`. Rejected:
  null is a legitimate runtime value — the failed-`OPTIONAL MATCH`
  reuse idiom (`… OPTIONAL MATCH (x)-[r]->(y) WHERE … MATCH (x)-[q]->(y)`)
  would become an error instead of the zero rows Neo4j gives.
- **No ADR: leave the spellings unpinned.** Rejected: several spellings,
  two semantics, one silent — the parallel state AGENTS.md bans; LABS-863
  exists because the divergence was found by accident.

## Consequences

- **Observable change on acceptance (stated loudly): queries that return
  rows today return zero.** The var-length `MATCH p = (a)-[*]->(b)`, the
  quantified `-->+`, the `ANY SHORTEST` selector, and the pre-bound-trail
  shapes with a null-bound end all answer zero rows; positive controls
  (a real node binding) are unchanged, and the start side and single-hop
  family are already conforming. No pinned test asserts the divergent
  end-side behavior today, so no existing gate moves — the pins below
  are additions.
- **Implementation: the one-match-arm class, at the reconciliation
  sites.** `gpm_bind_node`'s Fresh arm splits (entering-row null ⇒
  refuse the binding; machinery-seeded null ⇒ bind), with the same split
  at the walk's end/target accepts and the exact-trail pin's far-node
  reconciliation; `Route::bound` reads the full classification. The
  null-source distinction is the one design decision the implementer
  owns; qpp.rs's seeding contract makes either spelling of it sound.
- **Pins:** end-side pins per spelling (var-length, quantified, `ANY
  SHORTEST`, pre-bound-trail) mirroring #644's
  `null_bound_end_matches_nothing_on_both_engines`, plus one for the
  route anchor's classification read.
- **ADR-0335 overlap — the end-side ruling constrains the rel side by
  the same reasoning but does not decide it.** ADR-0335's own text is
  untouched: its two sites keep fresh semantics, and the walker's
  generalization was never ADR text. But the ruling's split —
  machinery-seeded fresh, entering-row mismatch — is exactly the split
  the rel-side divergence needs: the pinned GPM rel-side rebind (the
  gpm_tests pin's GPM arm, asserting rows today) is the same
  entering-row null class and should be expected to flip to the
  family's zero rows as its own follow-up decision, updating that pin's
  GPM arm. Accepting this ADR signals that direction; it does not flip
  the rel side here.
- **Follow-up work either way.** Under (a): the match arms above, the
  route-anchor alignment, the new pins, and the rel-side decision
  ticketed on top of this ruling. Under (b): pins for each divergence,
  the `prebound()` doc comment stating the deliberate divergence from
  Neo4j, and the rel side codified in the same stroke.
