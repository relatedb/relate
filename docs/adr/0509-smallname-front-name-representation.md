---
type: adr
area: cypher
claim: 06c2c0fae19f90f1c1dee156c5742b5d2ec20cf3
name: 0509-smallname-front-name-representation
status: proposed
tags:
  - parser
  - perf
title: SmallName — the query front-end's name representation (inline u64 / catalog id / spilled Arc<str>)
REFERENCES:
  - "[[adr/0103-engine-surfaces-testing-layout]]"
  - "[[adr/0182-unicode-identifiers]]"
  - "[[adr/0243-query-latency-suite]]"
  - "[[adr/0245-query-latency-baseline-environment-and-gate]]"
  - "[[adr/0257-query-input-limits]]"
  - "[[adr/0505-parse-pipeline-token-and-string-representation]]"
---

# ADR-0509: SmallName — the query front-end's name representation (inline u64 / catalog id / spilled Arc<str>)

## Context

The parse+lower pipeline is allocation-bound. The 2026-11-03 profiling
(a
2,103-statement corpus: 1,819 openCypher TCK statements + 284 workload
queries) measured 295 allocations per statement end to end, of which
front-end **lowering adds +119** on top of the parser's 176; roughly
40–50 % of parse+lower CPU is libc heap traffic, and on the
sub-millisecond half of the GBL SF0.1 latency suite ([ADR-0243](./0243-query-latency-suite.md), [ADR-0245](./0245-query-latency-baseline-environment-and-gate.md))
parse+plan is 30–100 % of wall time. [ADR-0505](./0505-parse-pipeline-token-and-string-representation.md) (the representation
predecessor: borrowed token text, owned AST strings, alloc-free keyword
lookup, FxHash scope maps) removes the token and operator allocation
churn, but leaves the per-statement scope-map keys and the AST/IR name
fields as owned `String`s — the residual this decision attacks.

The SmallName spike (commit `218ef6617`, merged `1eccc080c`; prototype
`cypherlite/cypherlite-front/tests/smallname/prototype.rs`, bench
`cypherlite/cypherlite-front/examples/smallname_bench.rs` with the
committed `# Findings` block) measured the name stream of the same
corpus: 53,781 occurrences, 3,873 distinct, 87.9 % fitting a 7-byte
ASCII inline arm, distinct names per statement median 8 / p90 35, each
distinct name repeating ~14x. Replaying that stream through
per-statement symbol tables:

| strategy | allocs/stmt | ns/lookup |
| --- | --- | --- |
| `HashMap<String, u32>` (SipHash baseline) | 17.16 | ~39 |
| `FxHashMap<String, u32>` (hasher control) | 17.16 | ~23 |
| `FxHashMap<SmallName, u32>`, re-classified per lookup | 5.90 | ~20 |
| `FxHashMap<SmallName, u32>`, pre-classified (wired shape) | 2.81 | ~13 |
| `Vec<(SmallName, u32)>` linear scan, pre-classified | 1.00 | ~34 |

Facts that bound the choice:

- **The win exists only in the wired shape.** Re-classifying from text
  at every map lookup gives the whole representation win back to the
  hasher control (~20 ns vs ~13). Classification must happen exactly
  once — when the parser builds the AST node from the identifier token —
  and every downstream consumer must clone the `SmallName`, never
  re-derive it from text.
- **No struct-width win.** `size_of::<SmallName>() == size_of::<String>()
  == 24` (asserted in the spike's tests): the wins are allocations
  avoided (the inline arm needs no heap) and hashing/equality over a
  bare `u64`, not smaller structs.
- **The linear-scan Vec table is a no-go as the lookup structure** (~34
  ns pre-classified, slower than the Fx control; the p90-35 statements
  scan too much). Its 128 B/stmt shape (vs ~1,500 for every hash-table
  variant, 11.7x fewer bytes) is a memory-shape option only.
- **Methodology this decision carries** (and the implementing stages
  must reuse): the counting allocator overrides `realloc` (`Vec` growth
  is realloc traffic; the `GlobalAlloc` default re-bills it as
  alloc+dealloc); strategies run one per process (a shared warm state
  biases order); timed figures are per-pass medians over fixed-duration
  loops.
- **The AST outlives the input** ([ADR-0505](./0505-parse-pipeline-token-and-string-representation.md)): `cypherlite-ir` embeds
  parser clauses, the planner matches them, the executor and ontology
  crates keep parsed graph-type bodies — so a borrowed representation
  drags the input lifetime through IR and the planner.
- **Names reach user-visible output byte-identically today**: plan
  render, error messages, TCK error details. openCypher TCK 3892/3892
  is the floor gate.
- **Catalog ids already exist, in the executor.** The core store's
  interner (`cypherlite-executor/src/store.rs`) assigns per-store `u32`
  ids for labels, relationship types and property keys at write/execute
  time (`intern`/`get_id`); the front-end lowering entry points
  (`parse_and_lower*`) today take no catalog.
- **Dependency boundaries ([ADR-0103](./0103-engine-surfaces-testing-layout.md)).** `cypherlite-parser` depends only
  on `cypherlite-model`; `cypherlite-ir` on parser + core;
  `cypherlite-front` on error, model, parser, core, ir. A type carried
  by AST fields must live somewhere the parser can depend on (R1).
- Unicode identifiers ([ADR-0182](./0182-unicode-identifiers.md)) and backtick-quoted names never fit the
  inline arm's ASCII identifier shape; the 4 MiB query-size cap
 ([ADR-0257](./0257-query-input-limits.md)) bounds adversarial input to the per-statement tables.
- The plan cache (LABS-790) removes the parse+plan tax only for repeated
  statement texts; this decision cuts it for every statement. The two
  compose.

## Decision

### 1. The representation: three arms, classified once at AST build

Adopt the spike's shape unchanged:

```text
SmallName = Inline(u64)       // <=7 ASCII identifier-shape bytes:
                              //   name bytes little-endian in 0..7,
                              //   byte length in byte 7 (Pascal tag)
          | Id(u32)           // per-store catalog id: label,
                              //   relationship type, or property key
          | Spilled(Arc<str>) // >7 bytes, non-ASCII, or quoted-only
                              //   shapes (spaces, escapes, hyphens, …)
```

Classification is one total function (`SmallName::from_name`): a given
`&str` always lands in the same arm, so the derived `Eq`/`Hash` are
name-equality (the inline encoding is injective — 7 payload bytes plus a
disjoint length tag, swept exhaustively for 1–2 char names in the
spike's tests; `Spilled` compares by `str` value; no name reaches two
arms).

**Carried by:** the AST's name fields (`name`, `variable`, `labels`,
`key`, relationship-type fields in `cypherlite-parser/src/ast.rs`), the
IR's name fields (`variable`, `labels`, `types`, `alias`, … in
`cypherlite-ir/src/ast.rs`), and the front-end's per-statement scope
maps and sets (`pattern_vars`, `output_vars`, `call_scope`, the
`validate/*` scopes) as `SmallName` keys on the FxHash tables [ADR-0505](./0505-parse-pipeline-token-and-string-representation.md)
established.

**Staying `String`:** error paths (`ParseError`/`EngineError` own their
message text; names are formatted in via `Display`), wire and plan
rendering (renders text, stores nothing), and catalog-facing storage
(the `cypherlite-catalog` name sets, the executor interner's `Arc<str>`
keys, WAL/snapshot payloads). `SmallName` is a per-statement front-end
representation: it is never persisted, never on the wire, and store
interner ids remain per-store runtime ids.

**The wired-shape constraint is the design's load-bearing rule:** the
parser constructs each `SmallName` once, at AST-node construction from
the identifier token's text; lowering, scope maps and IR cloning carry
the value (inline arm: a `u64` copy; spilled arm: an `Arc` refcount
bump). No code path re-classifies name text at map-lookup time.

### 2. The `Id(u32)` arm: catalog ids resolved at lowering, loud on invariant breaks

The arm carries the per-store catalog id space the executor's interner
already owns (labels, relationship types, property keys).

- **Resolution point:** lowering. The `parse_and_lower_*` entry points
  gain a read-only catalog view; label/rel-type/property-key names in
  read positions resolve through `get_id`. Lowering never interns:
  write clauses (`CREATE`/`MERGE`/`SET` introducing new vocabulary) keep
  the text arm and the executor interns at write time, as today.
- **Unknown-vocabulary policy:** an unresolved name is not an error — a
  `MATCH` on a never-seen label legitimately matches nothing (today's
  `get_id → None` semantics), so the name stays in its text arm and the
  executor's existing behavior applies unchanged. The loud rule governs
  the reverse direction: dereferencing an `Id` the catalog cannot name
  is an invariant violation — `expect` with the stated invariant (the
  `store.rs` `get()` precedent: "interned id is published before use"),
  never a silent fallback to empty text, a fabricated name, or a
  fabricated id.
- **Display:** the spike's bare-decimal `Id` rendering was a pinned
  placeholder and is dropped at promotion — `SmallName` ships no
  bare-decimal path. `Display`/`Debug` render the `Inline` and `Spilled`
  arms byte-identically to the input text. Once the `Id` arm is wired,
  every user-visible rendering of a name (plan render, error messages)
  resolves ids through the catalog's id→name table via the one
  canonical resolver adapter, keeping output byte-identical; `Debug`'s
  structured `Id(n)` form is internal diagnostics only, and the wiring
  change audits that it never reaches user-visible errors.

### 3. Spill policy: shared `Arc<str>`, deduped per statement

Spilled text is `Arc<str>`, deduped through one per-statement spill
table held by the parser/lowerer: a repeated long or quoted name
allocates once per statement and every further occurrence is a refcount
bump. The table dies with the statement — no global interner, no
cross-statement lifetime, no eviction policy. The measured 6.1x
allocation win does not depend on this dedupe (the bench's wired shape
allocated per occurrence); it shaves the residual further and keeps the
87.9 %-inline corpus's spilled repeaters cheap.

### 4. Crate placement: a new leaf crate, `cypherlite-names`

`SmallName` lives in a new tiny engine crate
`cypherlite/cypherlite-names` (the `cypherlite-splitter` pattern):
`cypherlite-parser`, `cypherlite-ir` and `cypherlite-front` each gain
one dependency edge on it — R1-legal, R5-legal, no new external
dependency, no R6 event. `cypherlite-front` cannot host it (front
depends on parser; AST fields changing means the parser must name the
type — a cycle), and `cypherlite-model` is the wrong home (see
Alternatives).

### 5. Migration shape: staged, four stages

1. **Stage 0 — the crate and the type.** `cypherlite-names` lands with
   `SmallName`, the classification function, and the spike's edge-case
   tests promoted verbatim (injectivity, totality, byte-identical
   round-trip, the 24-byte size fact). No behavior change; the `Id`
   constructor is crate-private so no `Id` values exist outside tests.
2. **Stage 1 — AST name fields.** `cypherlite-parser` AST name fields
   become `SmallName`; classification happens at AST-node construction
   (the wired-shape point). `Display` byte-identity keeps TCK, plan
   render and error text unchanged.
3. **Stage 2 — front-end scope maps.** `pattern_vars`, `output_vars`,
   `call_scope` and the `validate/*` scopes key on the `SmallName` the
   AST already carries. This is the stage that banks the measured win
   (2.81 allocs/stmt, ~13 ns/lookup shape).
4. **Stage 3 — IR name fields.** `cypherlite-ir` name fields become
   `SmallName`; the planner and plan render read through `Display`.
5. **Stage 4 — the `Id` arm wiring.** Lowering gains the catalog view,
   read-position label/rel-type/property-key names resolve to ids, and
   user-visible rendering switches to the resolver adapter. The largest
   semantic change, landed last and reviewable on its own.

Each stage is independently TCK-green with byte-identical rendering as
the gate, and independently revertible.

## Alternatives considered

- **Keep `String` (the status quo after [ADR-0505](./0505-parse-pipeline-token-and-string-representation.md)).** Rejected — the
  measured baseline: 17.16 allocations per statement and ~39 ns/lookup;
 the FxHash swap alone ([ADR-0505](./0505-parse-pipeline-token-and-string-representation.md) part 4) halves lookup time but leaves
  every per-statement scope-map key an owned `String`. The wired
  SmallName shape is 6.1x fewer allocations and 1.7x faster lookups than
  that Fx control.
- **Arena/bump allocation for AST names.** **Deferred** (user direction,
 2026-11-04), not rejected on merits — [ADR-0505](./0505-parse-pipeline-token-and-string-representation.md) already recorded the
  lifetime objection: the AST outlives the input (IR embeds parser
  clauses; the planner, executor and ontology crates hold lowered
  forms), so an arena must outlive every `Query` holder. Revisit if the
  post-feature profile still shows AST string cost.
- **A global string interner.** Rejected — a cross-statement table needs
  synchronization and either grows unboundedly or needs an eviction
  policy; front-end names include per-statement variables of unbounded
  variety, so interning pays a hash and store for names used once. The
  store vocabulary already has its own interner in the executor;
  per-statement tables die with the statement and need no locking.
- **The per-statement `Vec` symbol table (linear scan).** Rejected as
  the lookup structure by the spike data: ~34 ns/lookup pre-classified
  versus ~13 for the Fx map — at p90 35 distinct names per statement the
  scan dominates. Its 128 B/stmt memory shape (11.7x fewer bytes than
  any hash-table variant) stays on record as a memory-shape option; not
 adopted because the latency gate ([ADR-0245](./0245-query-latency-baseline-environment-and-gate.md)) bands time and peak RSS,
  and ~1.5 KB of transient per-statement map bytes is not a measured
  RSS cost.
- **`cypherlite-model` as the home crate.** Rejected — model is the
  graph data-model value crate (`CypherValue`, serde-facing);
  `SmallName` is a language front-end representation with a catalog-id
  arm, not a graph value type. A dedicated leaf keeps the parser's
  dependency surface narrow and the type's evolution (the `Id` resolver
  seam) out of the serde-heavy value crate. The zero-new-edges
  convenience (parser already depends on model) does not outweigh the
  purpose stretch.
- **One big swap instead of staged migration.** Rejected — the AST field
  change alone touches every name read site across parser, front, IR and
  the planner; folding scope maps, IR and the `Id` wiring into the same
  change makes it unreviewable and unbisectable. Staging keeps each step
  TCK-green, banks the measured win at stage 2, and isolates the
  semantic risk (the catalog-view signature change) in stage 4.

## Consequences

- **Status: Proposed.** The user rules on this proposal before the
  wave-2 code nodes dispatch; until Accepted, no `src/` change lands —
  the spike stays under `tests/` as the committed, measured prototype.
- A new workspace member `cypherlite-names`; parser, IR and front each
  gain one dependency edge (R1); no external dependency, no R6 event.
- Expected effect, to be verified per stage on the corpus harness with
  the spike's methodology (realloc-overriding counting allocator,
  per-process strategy isolation, per-pass medians): per-statement
  front-end name allocations drop from the 17.16 shape toward the
  measured 2.81, and scope-map lookups from ~23 ns (Fx control) toward
  ~13 ns.
- Byte-identical output is the gate at every stage: openCypher TCK
  3892/3892, plan-render tests and error-message tests unchanged;
  `Display`/`Debug` for the text arms round-trip byte-identically (the
  spike's tests promote with the type).
- The FxHash exposure note carries from [ADR-0505](./0505-parse-pipeline-token-and-string-representation.md): not
 collision-resistant, bounded by the 4 MiB query-size cap ([ADR-0257](./0257-query-input-limits.md))
  and the per-statement lifetime of the maps.
- Stage 4 changes the `parse_and_lower_*` signatures (a read-only
  catalog view parameter) — an internal API change only. No public Rust
  API, C ABI, CLI, Bolt/HTTP/MCP protocol, or documentation surface
  changes; no `docs/api/` update is required.
- `SmallName` never persists and never crosses the wire:
  WAL/snapshot/catalog storage stays string-keyed and store interner
  ids stay per-store runtime ids, so no migration or compatibility
  concern arises for durable data.
- If the user rules against any single decision point, only the
  affected stage is re-planned; the stages are independent by
  construction.
