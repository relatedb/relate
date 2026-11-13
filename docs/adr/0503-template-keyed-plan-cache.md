# ADR-0503: Template-keyed plan cache with stats-generation replanning

Status: Proposed

Claim: f3796c220bc3a44b36bebfc94dd9942309399261

## Context

Every RUN re-parses and re-plans its statement; there is no plan cache
anywhere in the engine. Measured on a 1.8M-relationship social-network
graph, in-process
(`perf-lab time`): `MATCH (n:User {id: $id}) RETURN n` — engine p50 17 µs,
EXPLAIN of the same statement p50 17 µs, execution ~1 µs (PROFILE hits=1).
The entire engine cost of a point read is parse+plan; over Bolt the same
query is 70 µs p50, so the plan cost is roughly a quarter of the whole
statement before the serve handoff. The point-read profile's real frames
are planner code (`check_match_clause`, `plan_gate_classify`, binding-table
clones). The 2026-10-31 engine-design session deferred a plan cache with a revival
condition — real repetition *and* a wall — and that condition is now met:
MCP/serve/embedded-agent workloads repeat a couple of statement shapes with
fresh literals, and the 17 µs parse+plan floor is 45.6 % of
`admit_statement` on point reads.

The session also fixed the design constraints, and this ADR is minted from
them rather than from a blank page:

- **No text-keyed plan cache.** The failure catalogs of SQLite, Postgres,
  SQL Server, Neo4j and DuckDB all argue against it (§4's literature
  anchoring). A raw-string key only hits exact retries — the rare case;
  agents emit literals, not parameters.
- **Plans are stats-coupled.** `plan_with_stats` reads live cardinalities
  under the publication read guard at plan time (ADR-0218), so a cached
  plan is a plan for the graph *as it was* when it was planned.
- **Last-N ≤ 16, not single-element** — agents interleave a couple of
  shapes (introspection + data reads).
- **Normalization lives in the lexer, on the token stream** — keyword-only
  case folding, typed literal placeholders, lists-as-one-param (Neo4j's
  `$__AUTO_INT_3__` positional-typed-key precedent); no variable renaming,
  no size threshold. Token-level is grammar-aware where a regexp is not.
- **Read-only classified statements only** — the query-type classification
  that already exists for result metadata (ADR-0162) is the gate; writes
  are excluded because they self-invalidate (every commit bumps the
  generation) and import streams would poison the cache.
- The result metadata (namespace, query type) is derived per statement —
  the cache must key the plan, never the metadata.

## Decision

A per-store plan cache on the facade's `Shared`, holding at most 16
entries, shared by every connection of the store. Admission
(`Connection::admit_statement`) consults it before parsing; entries are
inserted after a clean read-classified admission.

**Three-stage key.** (1) Each entry remembers the last raw statement text
that hit it plus that text's extracted literal values — an exact retry
compares strings and skips normalization entirely (cheap, low hit rate).
(2) The real key: the **auto-parameterized template**, produced by a
token-stream normalizer in the parser crate's lexer module. (3) The
template resolves to the plan.

**The normalizer** (`cypherlite-parser`, on the token stream of the
statement, lexed with the same `ParseOptions` the real parse would use):

- Every token renders as written — keywords included. The session's
  keyword-only case-folding constraint is **narrowed to nothing at token
  level**, for correctness: the dialect keeps keyword spellings as
  case-sensitive *names* — `(o:Order)` parses with the name as written
  (ADR-0169), `n.match` and `{match: 1}` bind by spelling, and the token
  stream cannot prove a keyword sits in a clause position rather than a
  name position. Folding `:order` onto `:Order` would serve one label's
  plan for the other (found in review: a critical wrong-rows collision),
  so `match …` and `MATCH …` keep distinct templates — a miss, never a
  wrong plan. Non-clause keywords (`TRUE`, `COUNT`, …) were never
  foldable for the same reason (`n.true` and `n.TRUE` are different
  property keys).
- Value-bearing literal tokens — `Integer`, `Float`, `String` — become
  **typed placeholders** `$__AUTO_INT_0__`, `$__AUTO_FLOAT_1__`,
  `$__AUTO_STR_2__`, with the type in the name (Neo4j's
  `$__AUTO_INT_3__` precedent: the type is part of the key, so `42` and
  `'42'` never share a template) and the index counting normalized
  literals in token order.
- A bracket group whose every element is a normalizable literal (or such a
  group, recursively) collapses to **one** `$__AUTO_LIST_n__` placeholder
  whose value is the whole list.
- Identifiers render through the canonical quoting policy
  (`quote_identifier`), parameter tokens as `$name`, skipped strings as
  quoted literals — the join is unambiguous: two statements share a
  template only if they parse identically.
- No variable renaming, no size threshold.
- **Grammar guards** skip normalization where a parameter is not
  grammatical or would change meaning: a `String` followed by `:` (map
  literal key — parameters are not map keys), a `String` preceded by the
  contextual `FIELDTERMINATOR` identifier (LOAD CSV requires a literal
  there), an `Integer`/`Float` preceded by `*` or `..` (var-length
  quantifier bounds take literal integers only), and any literal inside
  a function call's argument list — it feeds the front-end's constant
  folding (a temporal constructor's argument, string, list or map form
  alike, folds to a constant at lowering; a parameter there stays a
  per-row call, measured 3.5× slower on the GBL q8 shape before the
  guard). A statement carrying an interpolated string bails
  whole: its payload embeds expressions — including parameter
  references the token-level reserved-name check cannot see, which
  could collide with the rewritten text's own placeholders (found in
  review). `CYPHER <version>`
  integers need no guard: the rewritten parse fails, so those statements
  simply stay uncached. A skipped guard can only cost a cache miss, never
  a wrong plan.
- The pass also emits the **rewritten statement text** — the source with
  each normalized literal's byte span (this token's start to the next
  token's start, whitespace swallowed) replaced by its placeholder — and
  the extracted **values** in placeholder order.
- Statements that reference any `$__AUTO_*` parameter, and callers whose
  parameter map carries any `__AUTO_*` key, skip the cache entirely (no
  lookup, no insert), so a caller cannot collide with the reserved prefix.

**Correct-by-construction insertion.** On a miss, the statement parses,
plans and classifies exactly as today — the original text, unchanged error
fidelity. If the admission classifies it `QueryType::Read`, with
`write_route == false`, with no `EXPLAIN`/`PROFILE` preamble, with no
**dynamic name operand** in its plan (a dynamic label or property key:
LABS-709 validates those against bound *values* at lowering time — a
content-level check, an empty or null-byte name, that typed placeholders
cannot preserve — and a hit skips lowering, so caching one would let a
later statement's values escape that admission check; found in review),
and its text within a 64 KiB insert-side bound (the cache's memory bound —
a few maximal statements must not pin disproportionate derived memory
outside every budget; the lookup path has no size threshold and never
does), the entry is built by parsing and planning the **rewritten text**
with the extracted values bound as `$__AUTO_*__` parameters. Any literal position
whose parameter form is ungrammatical fails that rewritten parse and the
statement simply stays uncached — the failure mode of an over-eager
normalization is a miss, never a wrong plan. Both re-planning (below) and
first execution use this same parameterized plan shape. The session's
`CALL { … }`/`FOREACH` conservative exclusions (§7) ride the read
classification itself: the planner folds a `CALL { … }` subquery as
`Access::WRITE` and a `FOREACH` as a write clause, so neither can classify
`r` — the exclusion is enforced where it is owned, not re-encoded beside
the cache.

**Stats-generation coupling.** Every entry carries the store's
**stats generation** — a monotonic counter on `Shared`, advanced with
every data-state publish: the commit tail (`finish_applied_commit`, the
one tail every commit path funnels through), the follower's batch apply,
and graph replacement (a follow reload installs a different graph
instance). The advance rides *inside* the horizon publish — one
operation, so no future apply path can publish a data state and forget
the generation. Entries are stamped
under the publication read guard — the same guard that reads the planner's
live cardinalities — and checked under it, so an entry is reused only
while nothing has committed since it was planned. On mismatch the entry
**re-plans from its cached lowered AST** with fresh stats and updates
itself: Neo4j's replan-on-stats-drift equivalent, fail-closed at every
commit. Writes never insert, and every write commit invalidates every
reader through the advance.

**The plan is keyed, never the metadata.** On a hit, the gates and the
result metadata still derive per statement, from the cached plan under the
read guard: call-name collection, the preview gate, compile-time `CALL`
validation, the namespace and ontology gates, the query-type and
write-route classification, the ADR-0308 parameter-completeness check, the
statement-scoped notifications. The statement's extracted literal values
are merged into its parameter map before any of them run, so every
consumer downstream of admission (validation, dispatch, the PROFILE
estimate trees, the token budget) sees the values exactly as a
hand-written parameterized statement would.

**Plan trees never serve cached plans.** `EXPLAIN`-/`PROFILE`-prefixed
statements never insert (their templates carry the prefix, so they can
never hit either), and `Connection::explain_plan` bypasses the cache
entirely: the tree's seek lines keep spelling the statement as written
(`plan_node/seek.rs`'s own contract — "the statement's own spelling is the
plan's").

The generation counter — instance and type — lives in the facade
(`Shared`, `plan_cache`) rather than the catalog crate: every advance
site (commit tail, follower apply, reload replace) and every reader is a
facade path, and the counter must move on *data* commits, not only
catalog changes — the durable catalog crate would hold a fact it cannot
observe.

## Alternatives considered

- **A text-keyed plan cache (raw string → plan).** Rejected: the session's
  literature sweep (SQLite/Postgres/SQL Server/Neo4j/DuckDB failure
  catalogs) argues against text keys, and agents emit literals, so the raw
  key only hits exact retries — the rare case.
- **Embedder-held prepared-statement handles (the SQLite
  `sqlite3_prepare` model).** Rejected this round: it is a public-API
  change on every surface (Rust, C, CLI, Python, JS, Java, Go, Swift,
  Bolt) with per-surface lifecycle documentation and invalidation
  contracts — an ecosystem-scale change for a workload one transparent
  store-level cache serves with no new API. It remains the alternative on
  the table: its constant folding and value-derived selectivity (the
  "sniffing's benefit without its cost" note) are the stronger long-term
  design, and it should be revived if per-statement literal selectivity
  divergence ever shows up in profiles.
- **An AST-level rewrite (walk the parsed statement, replace literal
  expressions with parameter expressions).** Rejected: it needs a second
  normalizer beside the token-stream key — two spellings of one policy is
  exactly the parallel-state failure the code-shape rules forbid. The
  token pass produces the key, the rewritten text and the values in one
  walk; they cannot drift.
- **`commit_ts` as the generation.** Rejected: a follower's
  checkpoint+reload replays fewer records than the prior graph instance
  had commits, so equal timestamps can describe different data states. A
  dedicated counter bumped at every mutation site, replacement included,
  is alias-proof by construction.
- **A per-connection cache.** Rejected: the network surfaces open one
  connection per client, so per-connection caches duplicate compiled plans
  across a store's connections and never warm each other; the store-level
  cache costs one uncontended mutex lock (~20 ns) against the 17 µs it
  saves.

## Consequences

- Repeated read shapes skip parse+plan entirely. Point reads on hits are
  expected to drop from a 17 µs engine p50 to roughly the cost of one
  lexer pass plus the gate walk; before/after numbers from the
  query-latency suite (the in-repo harness,
  [`docs/testing/query-latency.md`](docs/testing/query-latency.md)) land
  in the PR.
- A cacheable miss compiles the rewritten statement once and executes
  that plan. The cache retains it only when its measured size fits the
  workspace pool's admission ceiling beside the statement reservation.
  A later miss tries again. A hit does not reserve the plan a second
  time; it reserves the statement workspace against the pool that
  already holds the plan, and a refusal leaves the entry cached.
  Statements that do not participate, and rewritten compiles that fail,
  compile the original text once and reserve that plan on the statement
  until it ends. A plan that does not fit that reservation is refused
  with the workspace limit.
- Writes are unchanged except for one lexer pass of template lookup
  (sub-µs); they never insert and they self-invalidate every reader.
- **Parameter sniffing, bounded:** a template's plan shape is fixed at
  insert time with the first statement's literal values visible to the
  planner's cardinality probes; later hits with skewed values reuse that
  shape — correct (the executor binds the fresh values) but possibly
  suboptimal, the same tradeoff Neo4j's plan cache makes, bounded by the
  replan-on-generation.
- An `IN` over a literal list may plan differently once auto-parameterized
  (the planner's constant folding sees a parameter instead of a literal
  set); the query-latency numbers in the PR record the measured effect.
- **Literal-shape recognizers must resolve parameters or the cache
  regresses their fast paths.** Found and fixed in this change: the
  exists-probe early exit (`RETURN count(DISTINCT …) > 0`) matched the
  threshold *literally* (`Expr::Int(0)`), so the auto-parameterized
  `> $__AUTO_INT_n__` silently fell back to the full-count walk — q28,
  a boolean-count shape, regressed 3.6× until `counted_existence`
  learned to resolve a parameter that binds the threshold (a caller's
  own `> $threshold` gains the same recognition). The constant-folding
  half of the same lesson could not be fixed that way — a folded
  temporal is a *value*, so a shared plan cannot fold it — and is
  handled by the normalizer's function-argument guard instead (the GBL
  q8 shape, `date("1990-01-01")`, regressed 3.5× before the guard).
  Any future recognizer that matches a literal shape on the AST carries
  the same obligation.
- A procedure-argument type error surfaces at execution instead of
  admission for cached shapes: a parameter's type is checked where it
  binds, not where it was spelled (the compile-time
  `static_expr_value` check sees a parameter and defers). Noted as an
  observable shift, confined to statements that were already errors.
- `EXPLAIN`/`PROFILE`-prefixed statements, `CYPHER <version>`-preamble
  statements, `CALL { … }`- and `FOREACH`-bearing statements, and
  statements mixing `$__AUTO_*` names stay uncached — never broken.
- The cache holds ≤ 16 entries of a lowered `Query` plus a `LogicalPlan`
  per store, behind a mutex on the admission path — and no entry is
  built from, nor raw text remembered above, a 64 KiB statement bound.
  When workspace admission is on, each retained entry also reserves its
  measured bytes (template, query, plan, raw text, and extracted values,
  rounded to the 4096-byte quantum) on the concurrent logical-workspace
  pool, under the same admission ceiling as new work. Count eviction,
  template replacement, and a cleared cache release that reservation.
  There is no separate plan-cache limit. Admission off keeps the count
  and text caps only. The no-size-threshold constraint governs the
  lookup, which neither bound touches.
- `plan_node/seek.rs`'s "this engine has no auto-parameterization" comment
  narrows to "plan trees never render cached plans".
- Staging: this lands the full design — three-stage key, lexer
  normalizer, stats-generation replanning, read-only gating — as one
  concern. The session's fallback (tokenizer/AST-level caching one level
  below the plan) is not needed.
