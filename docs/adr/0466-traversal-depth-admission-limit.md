# ADR-0466: Traversal depth is admission-limited, not left to the stack

Status: Proposed

Claim: 78c41c205cd2fe12b2df727bdc1f839acec4e012

## Context

The 27 October security review's finding 10 (LABS-610) is that a query
can ask for a traversal depth that aborts the process. An explicit
variable-length hop bound is walked recursively, and the native stack
grows with the bound. Measured through the real CLI on a 12k-node
indexed chain, with the statement `MATCH (a)-[*1..N]->(b) RETURN
count(*)`:

| Build / stack | Last accepted | First abort |
|---|---|---|
| Debug native | `*1..10000` | `*1..11000` |
| Release native | `*1..30000` | `*1..40000` |
| WASM, 16 MiB stack (debug) | ≈ 2,700 hops | — |
| WASM, 16 MiB stack (release) | ≈ 8,800 hops | — |

A stack overflow is an abort, not an error: the host process dies, the
store handle is lost, and no surface can report anything.

Two facts shape the fix.

- **An explicit bound is reachable in two syntaxes.** Besides the legacy
  bracket form (`-[*1..N]->`, `-[*..N]->`), a quantified path
  pattern carries a group quantifier (`((a)-[e]->(b)){1,N}`). The parser
  rejects only the *unbounded* group form under `REPEATABLE ELEMENTS`
  (`UnsafeRepeatableElements`, `parser/clauses.rs`). A bounded
  `{1,N}` is accepted, and it multiplies: `N=20` over the review's
  graph produced 4.2 million rows in 19 s, and `N=1e6` is an unbounded
  denial of service.
- **The parser's comment claimed a cap that does not exist.** It said a
  worst-case REPEATABLE ELEMENTS evaluation would enumerate "up to the
  engine's iteration cap". No such cap exists on that route; the only
  real bound is the explicit quantifier. The comment is corrected in
  this change.

There is an existing, undocumented cap on the *unbounded* classic walk:
`cypherlite-executor`'s `DEFAULT_MAX_HOPS` (32, `ctx.rs::hop_bounds`).
`docs/reference/cypher-syntax.md` said an unbounded `[*]` "has no hop
limit in `MATCH`", which is false for classic `MATCH`: it stops at 32.
Only the `MATCH p = shortestPath(...)` route (ADR-0186) explores without
a hop cap, bounded instead by the relationship count. The reference is
corrected here.

Neo4j itself has no traversal-depth ceiling. This is a deliberate
dialect divergence forced by CypherLite's embedding story: the engine
runs on caller-owned native stacks, including a 16 MiB WebAssembly
stack, and cannot rely on a server-sized thread stack.

## Decision

1. **Default limit 500.** `cypherlite_core::MAX_TRAVERSAL_DEPTH` is 500
   hops: an explicit `max_hops` (a `RelPattern`'s `max_hops`, including
   the postfix quantifier form, or a `QuantifiedPathPattern`'s
   `quantifier.max`) above
   it fails at statement admission. 500 is five times the largest
   legitimate explicit bound anywhere in the repo, corpus, and docs
   (100), and far below every measured abort bound, including the
   16 MiB WASM stack.
2. **A raisable knob.** Every limit must be raisable and self-explaining:
   `OpenOptions::max_traversal_depth: Option<u32>`,
   `--max-traversal-depth N`, and `CYPHERLITE_MAX_TRAVERSAL_DEPTH`.
   Resolution is the flag or `Some(n)`, then the environment, then 500,
   resolved in the open preflight before the store lease (the CSV-policy
   shape). Valid values are 1 through **2000**
   (`MAX_TRAVERSAL_DEPTH_CEILING`): the ceiling leaves headroom below the
   smallest measured abort (≈2,700 on the debug WASM stack). Raising it
   is a deliberate act; values above the ceiling risk a stack overflow
   on small-stack surfaces, and the documentation says so.
3. **Enforcement at lowering.** A new
   `validate/structure.rs::validate_traversal_bounds` in
   `cypherlite-front` is part of the per-statement validation battery,
   so `CALL { }`, `WHEN`/`ELSE`, FOREACH, `EXISTS`/`COUNT`/`COLLECT`
   subqueries, and UNION/NEXT arms are covered. It scans every
   `PatternElement::Relationship` and, recursively, every
   `QuantifiedPathPattern` even inside another quantified group. The
   expression visitor covers pattern predicates, pattern comprehensions,
   and the `shortestPath()`/`allShortestPaths()` expression. An
   unbounded upper bound is exempt: the classic walk is already capped at
   32. The check runs before the planner, so no walker ever sees a
   rejected statement.
4. **One typed error, protection framing.** The rejection is
   `ErrorKind::TraversalDepthExceeded` (SyntaxError class), raised as
   `Error::syntax` at the lowering boundary — Bolt
   `Neo.ClientError.Statement.SyntaxError`, HTTP 400/`42N06`. The message
   names the requested bound, says what the limit protects, says it is
   not a database limit, and names the knob and its ceiling:
   `This query allows paths up to 1,000,000 hops. This is a safety limit
   protecting against excessive traversal (not a database limit) — lower
   the path bound, or raise it with --max-traversal-depth (up to 2000).`
5. **Documentation.** `docs/limitations.md` gains a "Traversal depth"
   section; `docs/reference/cypher-syntax.md` states the 500-hop
   admission limit and the real 32-hop classic-walk cap;
   `docs/reference/cli-flags.md` and `docs/api/{rust,bolt,cli}/spec.md`
   document the knob and the error.

## Alternatives considered

- **A default of 10,000 (the audit's placeholder).** Rejected: it sits
  at the debug-native abort edge (10,000 accepted, 11,000 aborted) and
  about four times over the debug-WASM bound, so the default would still
  crash the smallest supported surface.
- **Default 1,000.** Rejected: no legitimate bound in the repo exceeds
  100, and a lower default keeps the knob's "raise it deliberately"
  story honest. 500 gives five times headroom over all real usage.
- **A hard, non-raisable cap.** Rejected: legitimate analytical queries
  over long chains exist, and a silent stack abort is not an acceptable
  way to say no. The knob keeps the limit a policy, not a language
  feature.
- **Enforce in the executor's walker.** Rejected for this change: the
  walker is the wrong boundary. Admission already owns statement
  validity, a rejected statement should not be planned, and the
  executor's hop machinery has several entry points (classic, GPM,
  shortest route). One front-end check covers all of them.
- **Only reject the REPEATABLE ELEMENTS bypass.** Rejected: the bypass
  is one spelling of the same hazard; the classic bracket forms needed
  the limit too.

## Consequences

- A query with an explicit bound above 500 fails with a clear syntax
  error instead of risking an abort. The previous behavior for
  `*1..10000` on a debug build was a process abort.
- The default does not change any statement in the repo, the CIP/TCK
  corpus, or the docs: the largest explicit bound there is 100.
- A query that legitimately needs more raises
  `--max-traversal-depth` (up to 2000, with the documented platform
  caveat) or sets `CYPHERLITE_MAX_TRAVERSAL_DEPTH`. Embedded callers set
  `OpenOptions::max_traversal_depth`.
- The default/ceiling are a dialect divergence from Neo4j, which has no
  such limit. A Cypher query ported from Neo4j with an explicit bound
  above 500 needs the knob on CypherLite.
- The REPEATABLE ELEMENTS `{1,N}` bypass is closed by the same check;
  the parser's inaccurate "iteration cap" comment is corrected.
- The unbounded classic-walk cap (32 hops) becomes documented; it was
  always the behavior. The `shortestPath` MATCH route remains uncapped
  (ADR-0186), still bounded by the relationship count, not by this
  limit.
