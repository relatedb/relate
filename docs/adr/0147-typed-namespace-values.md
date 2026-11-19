---
type: adr
area: engine
date: 2026-09-28
deciders: CypherLite maintainers
name: 0147-typed-namespace-values
status: proposed
tags:
  - namespaces
  - values
title: Public namespace values are typed
REFERENCES:
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0117-namespace-selection-apis-and-bolt]]"
  - "[[adr/0148-shared-element-id-parser]]"
---

# 0147. Public namespace values are typed

## Context

`cypherlite::QueryResult.namespace` ([ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7) was a stringly `String`
whose real value was stamped later by convention (`Connection::
stamp_namespace` overwrote a `""` sentinel written by
`From<ExecResult>`), while the crate exports a typed `Namespace`. An
unstamped result was representable and indistinguishable from a stamped
one. The same review flagged `Connection::namespace()` returning a stringly
`&str` while every other public namespace value (the field on `Connection`,
the plan's `use_target`, `Graph::namespace()`) is the typed enum. Both were
flagged in review of the fixed-namespaces work ([ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md)/[ADR-0117](./0117-namespace-selection-apis-and-bolt.md)).

## Decision

Public namespace values are typed `Namespace`. `QueryResult.namespace` is
typed `Namespace`: the `From<ExecResult>` impl and the `""` sentinel are
gone; construction takes the namespace (`QueryResult::from_exec(result,
namespace)`, `explain_result(…, namespace)`), so the stamp is unavoidable
and there is no unstamped state. The same rule covers the session getter:
`Connection::namespace()` returns the `Copy` `Namespace`, not `&str`.
Readers that need the wire string (Bolt's `db` echo, the Python binding's
`str` return) use `Display`/`as_str` at the boundary.

## Alternatives considered

- **Keep `namespace: String` and document the stamping convention.**
  Rejected: the convention was exactly what review flagged — a public typed
  `Namespace` existed and the field bypassed it; an unstamped result stayed
  representable.
- **`Option<Namespace>` to model the unstamped state.** Rejected: no
  consumer wants an unstamped result; making the unset state explicit would
  push a `None` case onto every reader (Bolt's `db` echo, tests) for a
  state that never legitimately occurs.
- **Keep `Connection::namespace() -> &str`.** Rejected: the same stringly
  seam review flagged on the result field — the typed `Copy` enum is free
  at the getter, and every reader that needs text renders at the boundary.

## Consequences

- `QueryResult.namespace: Namespace` and `Connection::namespace() ->
  Namespace` are breaking changes to the public Rust surface (field type
  `String` → `Namespace`; getter `&str` → `Namespace`);
  `docs/api/rust/spec.md` is updated. Rust tests compare against
  `Namespace::*` variants; the Bolt `db` echo renders with `to_string()`
  and the wire format is unchanged, and the Python binding still returns
  `str` (rendered at the boundary), so Bolt and Python behavior is
  untouched.
- `Connection::stamp_namespace` and the `From<ExecResult>` sentinel are
  deleted; every `QueryResult` is stamped with the resolved namespace at
  the `Connection::dispatch` call sites (`QueryResult::from_exec(r,
  namespace)`), so a result handed out by `Connection::query` always
  carries its real namespace by construction. The execution helpers
  (`commit_optimistic`, `query_in_txn`, `execute_read_at`) return the raw
  `ExecResult` and take no namespace parameter.

## Later references

- [ADR-0148](./0148-shared-element-id-parser.md): the shared element-id shape parser, split out of this file's
  original draft (one decision per file).
