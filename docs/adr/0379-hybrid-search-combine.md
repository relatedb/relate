# ADR-0379: Hybrid search `COMBINE` behind the `hybrid-search` gate

Status: Accepted

Claim: 9ddeb5a417f3977c391d314e2142eac1c06b7b43

## Context

Hybrid search `COMBINE`, accepted into the Cypher language but not yet part
of the pinned openCypher TCK floor, extends the `SEARCH` MATCH subclause
with a `<combined specification clause>`: `COMBINE ( <weighted source
clause>, ... ) USING WRRF [ WITH CONSTANT <expr> ] [ LIMIT <expr> ]`, where
each weighted source is an index specification or a nested combined one,
each with an optional `WEIGHT`. Per ADR-0371 it ships behind the
`hybrid-search` row of the `CYPHERLITE_PREVIEW` registry.

The existing SEARCH pipeline modeled exactly one index specification per
clause: a flat `SearchClause { kind, index_name, for_expr, analyzer,
inner_where, offset_expr, limit_expr, score_var }` in the parser AST and
the IR, consumed by the front validators/lowerer, the planner binder, and
`exec::vector::search_match_clause`. The construct's grammar replaces that
body with a `<source specification clause>` that is either an index or a
(nestable) combined specification, so the flat shape no longer fits.

## Decision

Adopt the hybrid-search canonical grammar and WRRF semantics, gated:

- **AST/IR shape**: one enum, not parallel fields —
  `SearchClause { binding_var, spec: SearchSpec, score_var }` with
  `SearchSpec::Index(SearchIndexSpec) | Combine(SearchCombineSpec)` and
  `SearchCombineSpec { sources: Vec<SearchWeightedSource>,
  constant_expr, limit_expr }`. Every existing consumer (front
  validators/lowerer, IR walkers, planner binder, pushdown liveness,
  executor) reads the source tree through it; the per-index search
  validations run for every index specification in the tree, recursively.
- **Gate (ADR-0371)**: the engine's statement admission threads the
  store's resolved `PreviewFeatures` into a new front entry,
  `parse_and_lower_statement_with_preview(src, preview)`: a statement
  whose AST uses a `COMBINE` source requires `PreviewFeature::HybridSearch`
  before anything of it lowers or plans, failing closed with ADR-0371's
  authored error. The ungated `parse_and_lower_statement` stays the
  dialect-floor entry for callers without a store. This is the first
  production reader of `Shared.preview`; its interim
  `cfg_attr(not(test), allow(dead_code))` is gone.
- **Semantics**: per incoming row of the driving table, each source yields
  its own ranked (score-descending, id-ascending), SKIP/OFFSET/LIMIT-
  windowed candidate list; a source's rank-`i` entry (0-based) contributes
  `weight / (constant + i + 1)`; an entity missing from a source gets
  nothing from that source; the fused list is ordered by fused score
  descending, ties on entity id ascending (the proposal removed its
  documented tie-breaker — implementation-dependent), then the combined
  `LIMIT` keeps the n highest. `SCORE AS` exposes the fused score as FLOAT.
- **Where the proposal is silent or defers to implementation**, the
  engine-consistent choices are: fewer than two sources is a parse-time
  syntax error with the proposal's own message; WEIGHT/CONSTANT type
  errors carry the 22N01 wording with a `SEARCH WEIGHT`/`SEARCH CONSTANT`
  suffix; a source-kind mismatch against the first source errors with the
  proposal's own wording; an absent (NULL or non-list) VECTOR source
  query contributes no ranks instead of nulling the whole fused result
  (the top-level OPTIONAL behavior is unchanged).

## Alternatives considered

- **Ship `COMBINE` ungated, widening the dialect floor per PR.**
  Rejected: ADR-0371's core reason — the pinned TCK corpus is the language
  contract (`docs/tck/compliance.md`); the syntax would be user-visible
  before it is contractual.
- **Thread `PreviewFeatures` into `cypherlite_parser::parse` itself, so
  the parser tokenizes `COMBINE` only under the opt-in.** Rejected: the
  pure `parse(src)` signature is public API used across crates, surfaces,
  and tests; threading a set through it changes every call site for no
  observable gain — the engine-visible contract ("a COMBINE query fails
  closed with the authored preview error while disabled, before anything
  plans or executes") is met at the front parse entry the engine threads.
  The gate deliberately sits at the front layer's parse/dispatch point,
  not the token level.
- **Lower `COMBINE` to the showcase's hand-crafted `UNION ALL` +
  aggregation query.** Rejected: the combined specification is evaluated
  once per incoming row with per-row WEIGHT/CONSTANT expressions, which a
  statement-level rewrite cannot express, and the executor would re-rank
  the fused results anyway. A direct fused-candidate computation in
  `exec::vector` (~150 lines, reusing `ranked_better_cmp`,
  `eval_window_int`, and the existing index candidate scans) is smaller
  than any faithful rewrite.
- **Score-based fusion (RSF) beside WRRF.** Rejected: a future extension
  of the proposal, outside its accepted scope; the `<result fusion
  specification>` slot keeps `USING WRRF` as the only method.

## Consequences

- `COMBINE` is user-visible only with `CYPHERLITE_PREVIEW=hybrid-search`;
  queries using it are not portable to the pinned floor and may change as
  the construct evolves.
- The cip-eval stretch scoreboard gains the `hybrid-search-combine` key
  (10/10 doc-derived scenarios,
  `testdata/cip-eval/hybrid-search-combine/`); its ratchet test sets the
  opt-in in its own test process before any scenario store opens (noted
  in the ratchet test).
- When the TCK pin moves to include hybrid search `COMBINE`, the gate
  check, the registry row, and this ADR's gating rationale are removed in
  that same change (ADR-0371's scope policy).
- The front layer now has two parse entries (gated and ungated); the
  gated one is the only one that consults the preview registry, and every
  engine statement path goes through it via connection admission.

## Later references

- ADR-0378 moves the resolved preview set off `Shared.preview` onto the
  store-level executor services; the `COMBINE` parse gate now receives the
  set from `Graph::preview()` under the admission read guard. The gated
  front entry and the hybrid-search registry row are unchanged.
