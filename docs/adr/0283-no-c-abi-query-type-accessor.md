---
type: adr
area: surface
date: 2026-10-05
deciders: CypherLite maintainers
name: 0283-no-c-abi-query-type-accessor
status: accepted
tags:
  - ffi
  - result-metadata
title: No C ABI accessor for QueryResult.query_type in this change
REFERENCES:
  - "[[adr/0117-namespace-selection-apis-and-bolt]]"
  - "[[adr/0162-query-type-on-result-and-bolt]]"
  - "[[adr/0285-c-abi-result-metadata-accessors]]"
---

# 0283. No C ABI accessor for QueryResult.query_type in this change

- **Renumbered**: from 0163 (shared-number cleanup)

## Context

[ADR-0162](./0162-query-type-on-result-and-bolt.md)'s Consequences promise that the implementing change ships "a C
ABI accessor with `docs/api/c/` updates (language clients wrap the C ABI
only)" alongside the Rust and Bolt surfaces, all in the same change. The
implementing branch ships `QueryType` and `QueryResult::query_type` on
the Rust API and the `type` key on the Bolt RUN SUCCESS summary, but no
C ABI accessor — and, on inspection, it should not:

- The product C ABI (`surfaces/cypherlite-ffi`) exposes no `QueryResult`
  metadata accessors at all. Its result API is exactly
  `cypherlite_column_count`, `cypherlite_row_count`,
  `cypherlite_column_name`, `cypherlite_value_json` and
  `cypherlite_destroy_result` (`surfaces/cypherlite-ffi/src/tabular.rs`);
  everything else is connection-, database- or transaction-level.
- The in-tree precedent for result metadata, the `QueryResult.namespace`
 field ([ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7), likewise has no C accessor — namespace selection
  reached the C ABI as connection calls
  (`cypherlite_connect_namespace` / `cypherlite_conn_set_namespace`),
  never as a result-field read.
- LABS-175's routing use case is already covered: a Neo4j driver reads
  the `type` key of the Bolt RUN SUCCESS summary, and an embedded Rust
  caller reads `QueryResult::query_type`. No JS, Java or Go client has
  asked for the value.

[ADR-0162](./0162-query-type-on-result-and-bolt.md) is Accepted and therefore immutable: narrowing its Consequences
is a new ADR (this one) plus a **Later references** note on the old one.

## Decision

**Ship no C ABI accessor for `QueryResult.query_type` in this change.**
[ADR-0162](./0162-query-type-on-result-and-bolt.md)'s C-ABI-accessor consequence is narrowed: the query type ships
on the Rust API and the Bolt RUN SUCCESS summary only. The rationale is
threefold:

1. **No result-metadata accessor precedent in the C ABI.** The FFI reads
   result rows (column count/name, row count, value JSON) and nothing
   else; the one existing result-metadata field, `namespace`
 ([ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7), has no accessor. `query_type` follows that precedent.
2. **The LABS-175 routing use case is covered without it.** The primary
   consumer — a Neo4j driver routing read-only vs read-write statements —
   reads the Bolt RUN SUCCESS `type` key; embedded callers use the Rust
   field.
3. **The follow-up is small and additive.** If a C-ABI client ever needs
   the value, a `cypherlite_result_query_type` accessor returning the
   1-char code (`"r"` / `"w"` / `"rw"` / `"s"`) is a self-contained
   addition; per the public-API rules, the change that adds it ships the
   `docs/api/c/` spec + description + example in the same change.

## Alternatives considered

- **Ship the accessor now (`cypherlite_result_query_type` in this
  change).** Rejected: it has no consumer — none of the JS/Java/Go
  clients that wrap the C ABI asks for the query type; the in-tree
  precedent says result-metadata fields do not get C accessors
  (`namespace` has none); and it is scope creep against LABS-175, whose
  routing use case the Rust and Bolt surfaces already cover.
- **Amend [ADR-0162](./0162-query-type-on-result-and-bolt.md) in place** to drop the C-ABI sentence from its
  Consequences. Rejected: Accepted ADRs are immutable — a later change
  is a new ADR plus a **Later references** note on the old one, which is
  exactly the mechanism this ADR uses.
- **Leave the drift.** Rejected: an Accepted ADR whose Consequences
  describe an unshipped surface sets an aspirational-Consequences
  precedent; the decision log must record what ships, not what was once
  hoped.

## Consequences

- The C ABI result API stays at `cypherlite_column_count`,
  `cypherlite_row_count`, `cypherlite_column_name`,
  `cypherlite_value_json` and `cypherlite_destroy_result`; the JS, Java
  and Go clients gain no query-type API. `docs/api/c/` is untouched by
  the explain-query-type feature; the `docs/api/rust/` and
 `docs/api/bolt/` updates [ADR-0162](./0162-query-type-on-result-and-bolt.md) requires still ship.
- `query_type` remains reachable on every planned statement — plain,
  EXPLAIN, PROFILE — through `QueryResult::query_type` (Rust) and the
  Bolt RUN SUCCESS `type` key, so LABS-175's routing decision is
  unaffected.
- A future `cypherlite_result_query_type` accessor returning the 1-char
  code is an additive follow-up requiring no engine change; the change
  that adds it ships the `docs/api/c/` spec + description + example in
  the same change, per the public-API rules.
- [ADR-0162](./0162-query-type-on-result-and-bolt.md) gains a **Later references** note naming this ADR; its
  original text is otherwise unchanged.
- This ADR changes no code, tests, or public surface beyond the decision
  log; TCK and cip-eval are unaffected.

## Later references

- [ADR-0285](./0285-c-abi-result-metadata-accessors.md): ships the additive follow-up this ADR deferred —
  `cypherlite_result_query_type` plus a `cypherlite_result_namespace`
 accessor for the [ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7 field, both result-owned
  `const char *`.
