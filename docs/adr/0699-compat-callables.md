---
type: adr
area: cypher
claim: ff3546dbe924b5534de1f10f014bd57f3fdc0692
date: 2026-11-26
deciders: Relate maintainers
name: 0699-compat-callables
status: proposed
tags:
  - dialect
  - procedures
  - functions
title: Compat callables — foreign procedure names re-implemented as Cypher bodies
REFERENCES:
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0371-preview-feature-gating]]"
  - "[[adr/0382-local-callables]]"
  - "[[adr/0497-catalogue-corpus-alignment]]"
---

# ADR-0699: Compat callables — foreign procedure names re-implemented as Cypher bodies

## Context

Client tools written against Neo4j call procedures Relate does not ship.
`neo4j-graphrag`'s `SimpleKGPipeline` writer, for one
([relatedb/relate#862](https://github.com/relatedb/relate/issues/862)), stops
on `ProcedureNotFound` for four names:

- `db.create.setNodeVectorProperty(node, key, vector)`
- `db.create.setRelationshipVectorProperty(relationship, key, vector)`
- `apoc.create.addLabels(nodes, labels)`
- `apoc.merge.relationship(start, relType, identProps, onCreateProps, end, onMatchProps)`

Every one of them expresses behavior Relate's dialect already has: dynamic
property keys (`SET n[$key] = v`), dynamic labels (`SET n:$(labels)`), and
dynamic relationship types (`MERGE (a)-[r:$(type)]->(b)`). The upstream
implementations agree: APOC's `apoc.merge.relationship` generates exactly that
`MERGE`, and Neo4j's `db.create.set*VectorProperty` is a property `SET` plus
fp32 packing. The gap is only the *name*.

The engine already carries three ways to answer a foreign procedure name
([ADR-0497](./0497-catalogue-corpus-alignment.md)):

1. **A Rust row producer** — the procedure is real engine code
   (`db.labels`, `tx.setMetaData`).
2. **A catalog-only row** — `executable(false)`: `SHOW PROCEDURES` lists the
   name for corpus compat (`db.awaitIndex`, `db.checkpoint`,
   `db.createLabel`, …), and a `CALL` fails.
3. **Nothing** — the name is `ProcedureNotFound`.

What is missing is a fourth way: *the call succeeds, with behavior small
enough to write as Cypher, without adding engine code*. APOC itself is the
precedent — its merge family is Java that generates Cypher — and Relate
already owns the machinery to substitute a Cypher body for a call site: the
local-callable rewrite ([ADR-0382](./0382-local-callables.md))
eliminates `DEFINE` blocks by rewriting every invocation into plain
pre-existing Cypher before lowering.

Local callables alone do not close the gap: a `DEFINE` block is
statement-scoped (the definition must ride the client's own query text) and
preview-gated behind `RELATE_PREVIEW=local-callables`
([ADR-0371](./0371-preview-feature-gating.md)). A remote client sending
`CALL apoc.merge.relationship(…)` over Bolt cannot use them, and asking every
tool to prepend definitions to its queries is the compatibility shim the
client was trying to avoid.

## Decision

A **compat-callables table** in the query front-end (`relate-front`) maps
foreign procedure names to Cypher bodies. When a parsed statement calls a
table name, the front-end injects the entry's `DEFINE`-equivalent definition
into the statement and the local-callable resolver
([ADR-0382](./0382-local-callables.md)) rewrites the call sites away —
the planner and executor never see the foreign name.

- **The table** is one `relate-front` static list of entries: name, kind,
  parameters (with defaults), yield columns, description, a `writes` flag,
  and the body as Cypher text. The body is parsed once (from the table's
  `DEFINE PROCEDURE …` spelling) and cached; it is ordinary reviewable
  Cypher, never hand-built AST.
- **Injection** runs in the one resolve-and-lower tail every parse entry
  shares, after the ADR-0257 depth guard and before the local-callable
  resolver — so no entry can omit it. In the gated entries the ADR-0371
  preview gate has already run on the user's AST, and an injected
  definition is engine-provided, not a user `DEFINE`, so it never trips
  the `local-callables` preview gate — a compat call works with
  `RELATE_PREVIEW` unset.
- **Precedence**: a user `DEFINE` of the same name wins (the injection skips
  names the statement defines anywhere in its tree); a store-registered
  procedure ([ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md)
  §10) wins (the engine's admission passes `Graph::has_procedure` — which
  answers whether the store HAS the name — as the injection's store
  predicate); only then is the table entry injected. The table is the last
  registry consulted, never the first.
- **Signature rows**: each table entry carries a matching row in the
  executor's `BUILTIN_PROCEDURE_SIGNATURES` — `executable`, `listed`,
  `writes` — so compile-time `CALL` validation, `SHOW PROCEDURES`, and the
  write classification read one policy. A drift-guard test pins the two
  tables together (names, parameters, yield columns, descriptions);
  engine classification lives in the signature table alone, membership and
  bodies in the front table. A signature row for a table entry declares no
  row producer, and the dispatch declines it by the front table's one
  membership check — a call that bypasses the rewrite reaches an authored
  error instead of a silent no-op, unreachable through any real surface
  (the injection runs in the shared tail) and loud if reached anyway.
- **An entry can be a deliberate no-op**: a body of `FINISH` makes the call
  succeed and do nothing — the form for foreign procedures whose behavior
  Relate consciously declines but whose *name* a client needs to call
  without error. This is distinct from the catalog-only row (call fails):
  choose a no-op entry when the corpus or client expects success, a
  catalog-only row when it only lists the name.

The four seed entries and their bodies (validated against the engine):

| Name | Body | Yields |
| --- | --- | --- |
| `db.create.setNodeVectorProperty` | `SET node[key] = value FINISH` | — (void) |
| `db.create.setRelationshipVectorProperty` | `SET relationship[key] = value FINISH` | — (void) |
| `apoc.create.addLabels` | `SET nodes:$(labels) RETURN nodes AS node` | `node` |
| `apoc.merge.relationship` | `MERGE (start)-[r:$(relType)]->(end) ON CREATE SET r += coalesce(onCreateProps,{}) ON MATCH SET r += coalesce(onMatchProps,{}) RETURN r AS rel` | `rel` |

Known divergences from upstream, accepted for the seed set: non-empty
`identProps` on `apoc.merge.relationship` is ignored (no Cypher map-splat
into a MERGE pattern exists; graphrag always passes `{}`);
`apoc.create.addLabels` takes a single node, not a node-or-list; the vector
procedures store the plain list with no fp32 coercion or non-null validation
(the property `SET` is the documented equivalent — embeddings are list
properties here). A later entry that needs more fidelity is a Rust producer,
form 1 — the table is for what Cypher can say.

## Alternatives considered

- **Rust row producers for the four names.** Rejected as the general answer:
  four more engine dispatch arms for behavior the dialect already expresses,
  and every future compat name grows the engine the same way. The Cypher
  body keeps the engine surface flat; a producer stays available when
  fidelity demands it.
- **Catch `ProcedureNotFound` and re-run the statement with `DEFINE` blocks
  prepended.** Rejected: a second parse and plan, an error round-trip, and a
  dependency on re-running text the engine has already compiled. Scanning
  the parsed AST for table names is one pass and costs nothing when no
  compat name appears.
- **Graduate `local-callables` from preview and tell clients to prepend
  `DEFINE` blocks.** Rejected: pushes the shim into every client, the exact
  friction the issue reports; the preview status of user-facing `DEFINE` is
  a separate decision this ADR does not make.
- **Catalog-only rows for the four.** Rejected: the writer needs the calls
  to *succeed*; a listed-but-failing name is the current failure with better
  signage.
- **Migrating the implemented `db.index.vector.query*` /
  `db.index.fulltext.query*` procedures onto the table.** Rejected for now:
  their `indexName` argument resolves at runtime, while `SEARCH` requires a
  literal index name at plan time — the `CallDivert` plumbing exists for
  exactly that runtime resolution, and the bodies are not expressible in
  Cypher until `SEARCH` accepts a dynamic index name (a plan-time/runtime
  resolution change that is its own ADR). Those procedures also work today
  and are corpus-pinned; the table's niche is names with no implementation
  behind them.
- **A body-template mini-language** (`$name` placeholders interpolated when
  the definition is built, with identifier quoting/sanitization around
  interpolated names). Deferred: the seed table needs none of it — nothing
  user-controlled ever reaches the text path, because the table is static
  maintainer-authored text and call arguments are bound as *variables* by
  the call-by-value rewrite, never spliced into the body, so there is no
  injection surface to sanitize. If the table grows body families (many
  entries sharing one body shape), revisit: the dedup tool would be
  compile-time const fragments first, a runtime template engine only if
  const composition stops paying.

## Consequences

- A remote client (Bolt, HTTP, MCP, CLI) calling a table name gets the
  behavior with no client change, no preview opt-in, and no engine code.
  `neo4j-graphrag`'s writer runs unmodified against `relate serve`.
- `EXPLAIN`/`PROFILE` of a compat call shows the rewritten subquery (the
  Cypher the body became), not a procedure node — an observable divergence
  from Neo4j's plan shape, documented here.
- The plan cache keys the rewritten plan under the client's original
  statement text. The seed entries are all write procedures and the cache
  admits only read plans, so no rewritten compat plan is ever cached; a
  later read-mode entry rides the same staleness contract every
  signature-dependent plan already carries (a store registration of the
  name takes effect at the next compile).
- Every new entry is a dialect decision: the table grows only through a PR
  that names the client contract it answers (this ADR is the template; the
  four seeds answer one issue). Entries are public behavior — each carries
  its changelog fragment.
- `SHOW PROCEDURES` lists the entries as `WRITE` procedures with
  descriptions naming their Cypher re-implementation, so the surface
  self-documents the compat policy — the documentation half of the
  originating issue's ask.
- No-op entries make "accept the name, decline the behavior" a declared
  table row instead of an engine arm; the catalog-only row remains the right
  form for names a client may only *list*.
