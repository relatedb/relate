---
type: adr
area: surface
date: 2026-10-05
deciders: CypherLite maintainers
name: 0285-c-abi-result-metadata-accessors
status: accepted
tags:
  - ffi
  - result-metadata
title: Result-metadata accessors on the C ABI
REFERENCES:
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0117-namespace-selection-apis-and-bolt]]"
  - "[[adr/0162-query-type-on-result-and-bolt]]"
  - "[[adr/0283-no-c-abi-query-type-accessor]]"
  - "[[adr/0502-typed-c-abi-v2]]"
---

# 0285. Result-metadata accessors on the C ABI

- **Renumbered**: from 0165 (shared-number cleanup)

## Context

LABS-176. Two pieces of `QueryResult` metadata exist on the Rust API but
never reached the C ABI, leaving the JS, Java and Go clients (which wrap
the C ABI only) blind to them:

- `QueryResult.query_type` ([ADR-0162](./0162-query-type-on-result-and-bolt.md)): the plan-time classification
  `Read` / `Write` / `ReadWrite` / `Schema` with wire codes `"r"`,
  `"w"`, `"rw"`, `"s"`, also echoed as the `type` key of the Bolt RUN
 SUCCESS summary. [ADR-0283](./0283-no-c-abi-query-type-accessor.md) explicitly deferred a C accessor as "a
  small, additive follow-up" requiring no engine change.
- `QueryResult.namespace` ([ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7): the statement's *resolved*
  namespace, which differs from the session namespace when a statement
  carries a `USE` clause. Namespace selection reached the C ABI as
  connection calls (`cypherlite_connect_namespace` /
  `cypherlite_conn_set_namespace`), never as a result-field read.

The C ABI has two string-ownership precedents
(`surfaces/cypherlite-ffi/src/tabular.rs`,
`surfaces/cypherlite-ffi/include/cypherlite.h`):

1. **Result-owned `const char *`** — `cypherlite_column_name` and
   `cypherlite_value_json` return pointers owned by the result, valid
   until `cypherlite_destroy_result`; there is no free call.
2. **Caller-freed `char *`** — `cypherlite_flush_result` and
   `cypherlite_reset_result` return strings the caller must free with
   `cypherlite_free_string`.

Both new values are fixed-membership, tiny strings known at result
construction ("construction is the stamp", [ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7), so either
convention is implementable; the choice is about which precedent fits a
result accessor and what the koffi/cgo/JNA binding code looks like
under each.

## Decision

1. **Two new C ABI functions**, both taking `cypherlite_query_result`:

   ```c
   const char *cypherlite_result_query_type(cypherlite_query_result result);
   const char *cypherlite_result_namespace(cypherlite_query_result result);
   ```

2. **Ownership convention: result-owned `const char *`.** The returned
   pointers are owned by the result and valid until
   `cypherlite_destroy_result`, exactly like `cypherlite_column_name` /
   `cypherlite_value_json`; there is no free call. Rationale: every
   existing *result* accessor is result-owned const — caller-freed mut
   exists only for handle-level APIs (`cypherlite_flush_result` /
   `cypherlite_reset_result`) — so a result-metadata accessor follows
   the result-accessor precedent. The binding code is also strictly
   simpler: koffi (`str` return), cgo (`C.GoString`) and JNA (`String`
   return) all copy the string while marshalling the return value, so
   under result-owned const each is a one-line binding with no
   subsequent `cypherlite_free_string` call and no leak-or-double-free
   discipline to document per client. Both value sets are small and
   fixed, so the pointers can be static strings with no per-result
   allocation at all.
3. **Value sets are pinned.**
   - `cypherlite_result_query_type` returns exactly `"r"`, `"w"`,
 `"rw"` or `"s"` — the [ADR-0162](./0162-query-type-on-result-and-bolt.md) set, aligned with the Neo4j
     measurements. A fifth value is a new ADR, not silent growth.
   - `cypherlite_result_namespace` returns the statement's resolved
 namespace: one of the [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) fixed namespaces `"neo4j"`,
     `"system"` or `"system_ontologies"`.
4. **Per-surface client property names.** Python wraps the Rust API
   natively (pyo3), not the C ABI; JS, Java and Go bind the two new
   symbols. The implementing client changes expose:

   | Surface    | `query_type`      | `namespace`      |
   | ---------- | ----------------- | ---------------- |
   | Python     | `QueryResult.query_type` | `QueryResult.namespace` |
   | JavaScript | `result.queryType` | `result.namespace` |
   | Java       | `Result.getQueryType()` | `Result.getNamespace()` |
   | Go         | `Result.QueryType` | `Result.Namespace` |

   Names follow each language's idiom: snake_case properties in Python,
   camelCase properties in JavaScript, getter methods in Java, exported
   struct fields in Go.

Per the public-API rules, the implementing change ships the
`docs/api/c/` spec + description + example for both functions in the
same change, and each client change updates its
`docs/api/<surface>/` docs and the CLI skill-bundle client reference.

## Alternatives considered

- **Caller-freed `char *` + `cypherlite_free_string`** (the
  `cypherlite_flush_result` precedent). Rejected: it breaks the
  result-accessor convention — every existing `cypherlite_query_result`
  accessor returns result-owned const pointers, and a caller would have
  to know that these two result strings, unlike column names and value
  JSON, need freeing. It also complicates every binding for no benefit:
  koffi/cgo/JNA copy the string on return regardless, so each client
  gains a mandatory `cypherlite_free_string` call (and a leak if it is
  forgotten) to return a value that is one of four or three static
  strings.
- **Ship only `cypherlite_result_query_type`** and leave `namespace`
  unreadable. Rejected: the two accessors are the same shape, the same
  ownership convention and the same construction-time stamp; `namespace`
 is the older gap ([ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7 shipped it on the Rust API with no C
  accessor) and a client that runs `USE`-carrying statements has no
  other way to learn which namespace a statement resolved to. Shipping
  one now and the other later is two C ABI/doc/client rounds for one
  decision's worth of work.
- **Defer again** ([ADR-0283](./0283-no-c-abi-query-type-accessor.md)'s stance: no C-ABI client has asked).
 Rejected: [ADR-0283](./0283-no-c-abi-query-type-accessor.md) deferred on "no consumer" grounds and explicitly
  framed the accessor as a small, additive follow-up; LABS-176 is that
  follow-up, scheduled now so the JS, Java and Go clients reach parity
  with the Rust and Bolt surfaces for result metadata rather than
  drifting further behind with each added field.
- **Return an integer enum instead of a string.** Rejected: the wire
  codes are already strings on every other surface (Bolt RUN SUCCESS
 `type` key, Neo4j Query API `queryType`, [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) namespace names); a
  parallel integer mapping would be a second encoding to keep in sync,
  and C ABIs in this project pass strings (`cypherlite_column_name`,
  `cypherlite_value_json`).

## Consequences

- The C ABI result API grows from `cypherlite_column_count`,
  `cypherlite_row_count`, `cypherlite_column_name`,
  `cypherlite_value_json` and `cypherlite_destroy_result` by exactly two
  functions; no existing signature changes, so the addition is backward
  compatible for all current bindings.
- The result-owned-const convention is now pinned for result-metadata
  strings generally: future `QueryResult` metadata fields exposed over
  the C ABI follow it unless a new ADR says otherwise.
- [ADR-0162](./0162-query-type-on-result-and-bolt.md)'s narrowed Consequences ([ADR-0283](./0283-no-c-abi-query-type-accessor.md)) are fulfilled: the
 `cypherlite_result_query_type` follow-up lands, and [ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7's
 `namespace` field becomes readable from every client surface. [ADR-0162](./0162-query-type-on-result-and-bolt.md)
 and [ADR-0283](./0283-no-c-abi-query-type-accessor.md) each gain a **Later references** note naming this ADR.
- Python, JavaScript, Java and Go each gain the two properties named in
  Decision §4; their implementing changes ship `docs/api/<surface>/`
  spec + description + example updates in the same change, per the
  public-API rules.
- No engine, TCK or cip-eval impact: both values are already computed at
  result construction; this ADR only exposes them.

## Later references

- [ADR-0502](./0502-typed-c-abi-v2.md): the typed C ABI v2 removes `cypherlite_value_json`, the
  `cypherlite_result_column_type` first-row stamp and the thread-local
  `cypherlite_last_error`. The result-metadata accessors this ADR added
  (`cypherlite_result_query_type`, `cypherlite_result_namespace`) keep their
  signatures; the rest of the result API, including `cypherlite_row_count`,
  `cypherlite_column_count` and `cypherlite_destroy_result`, is unchanged.
