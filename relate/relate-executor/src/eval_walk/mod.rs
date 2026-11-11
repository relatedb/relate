//! The eval-side pattern-walk engine — the walker behind pattern
//! comprehensions (`[p = (a)-[r]->(b) | …]`), pattern predicates
//! (`WHERE (a)-[r]->(b)`, the anchored probe and the limit-1 walk), and
//! the expression-form `shortestPath`/`allShortestPaths`: pattern.rs is
//! the walk itself, shortest.rs the shortest-path arm: the shortest route's
//! rows folded to one value, with this walker as its enumeration fallback. The anchored existence probe
//! (LABS-193/LABS-249) is not this engine's — it is cross-engine code
//! answering for this engine's `pattern_exists` and the exec exists
//! probe alike, so it sits at the shared tier with the rest of the
//! pattern-walk vocabulary (`crate::pattern_walk`'s `probe.rs`), both
//! readers importing it downward.
//!
//! Home judgment — the crate shared tier, a pattern-walk client like
//! the exec walkers (exec/expand, exec/pivot). This engine was born
//! inside the eval tree (`eval/pattern.rs`, `eval/shortest.rs`), which
//! left the module graph in the shape the pattern-walk vocabulary's
//! docs disavow: `pattern_walk` imports the expression core
//! (`eval`, `label_expr_matches`) downward, so a walker living IN that
//! core made `eval` import `pattern_walk` right back — the two-cycle
//! under the "both walk engines read the family without a module
//! cycle" claim. The walker leaves; `eval` is the expression core
//! strictly below `pattern_walk` (it imports no walk module), and both
//! walk engines — the exec walkers and this one — import the family
//! downward from `crate::pattern_walk` like every other consumer.
//!
//! The one edge that remains, stated precisely: the expression
//! dispatcher's pattern-expression arms and the `shortestPath()`
//! function form delegate UP to this engine
//! (`eval::eval` → `eval_walk`, exactly as its subquery arms delegate
//! to `exec` — `eval` ↔ `exec` is the crate's long-established
//! engine-delegation cycle among top-tier equals). So a transitive
//! cycle `eval → eval_walk → pattern_walk → eval` exists through those
//! arms; what closed here is the direct `eval` ↔ `pattern_walk`
//! two-cycle, and `pattern_walk`'s own edges all point strictly down
//! (row, the expression core, the property-policy walk) — no shared-
//! tier module imports an engine. Removing the transitive cycle would
//! mean the expression core cannot evaluate `PatternPred`/
//! `PatternComp`/`ShortestPath` expressions at all — a dispatcher
//! redesign (hooks or pre-resolution at every `eval` call site)
//! disproportionate to the claim it would buy; the delegation edge is
//! the established shape instead.
//!
//! Private submodule wiring only: `pub(crate)` re-exports give each
//! item one public path (`crate::eval_walk::…`), nothing re-exported
//! at the crate root.

mod pattern;
mod shortest;

pub(crate) use pattern::{collect_pattern_rows, pattern_exists};
pub(crate) use shortest::{eval_shortest, ShortestWant};
