---
type: adr
area: engine
date: 2026-10-01
deciders: CypherLite maintainers (agreed in conversation)
name: 0154-value-clone-cost
status: accepted
tags:
  - perf
  - values
title: "Value cloning cost: container values are O(1) to clone (Rc-backed)"
REFERENCES:
  - "[[adr/0144-graph-examples-differential-suite]]"
  - "[[adr/0155-bulk-writes-batched-unwind]]"
  - "[[adr/0170-shared-value-payloads]]"
---

# 0154. Value cloning cost: container values are O(1) to clone (Rc-backed)

## Context

The graph-examples differential suite ([ADR-0144](./0144-graph-examples-differential-suite.md)) surfaced a class of
quadratic behaviour on real datasets. The sharpest case: the evaluator reads
a variable with a deep clone of its bound value.

`cypherlite-executor/src/eval/mod.rs`:

```rust
Expr::Variable(n) => Ok(row.get(n).cloned().unwrap_or(Value::Null)),
```

Every variable read clones the whole value. For a large list or map, each
read is O(size) — so a query that touches such a binding k times is
O(k · size), regardless of what it does with the value.

Measured (release build), the same 2,000 list-index pairs at two graph
sizes:

| list size | user | system |
| --- | --- | --- |
| 2,000 | 1.78 s | 0.66 s |
| 20,000 | 29.33 s | 15.48 s |

Ten times the list length is sixteen times the user time for the same
number of accesses. At fixed size the cost is linear in accesses
(20/200/2000 accesses → 0.34 s / 2.67 s / 30.2 s), i.e. ~0.75 µs per
element cloned. There is no memory-pressure warning: nothing stays live —
it is allocate-and-drop churn, which is why the system time is large
(15 s of allocator/page-fault work at 20k).

The same mechanism multiplies through every operator that binds a value and
reads it more than once: property access on a large map, list indexing,
`UNWIND` over an indexed list, and multi-pattern `MATCH` where each
intermediate row re-clones its bindings.

The carry cost alone is dominant — the same query with the list bound but
never indexed:

| query (20k nodes collected) | user | system |
| --- | --- | --- |
| `UNWIND range(0,1999) AS k RETURN count(*)` (no collect) | 0.07 s | 0.02 s |
| `... WITH collect(n) AS ns UNWIND range(0,1999) AS k RETURN count(*)` | 9.16 s | 12.66 s |
| `... WITH collect(n) AS ns UNWIND range(0,1999) AS k WITH k, ns[k] AS a, ns[(k+1) % 20000] AS b RETURN count(*)` | 28.51 s | 16.79 s |

So ~9 s + 12.7 s of the cost is *only* cloning the bound list into each of
the 2,000 output rows (2,000 x 20,000 value clones); the index accesses add
the rest. `Row = IndexMap<String, Value>` is owned-value based, so there is
no contained fix short of the Rc-backed representation below.

## Landed mitigation (this change)

`Expr::Subscript` and `Expr::Property` now read a *variable* operand by
reference (`row.get(name)`) and clone only the element, instead of
evaluating the operand (which deep-cloned the whole container) and then
indexing. Measured on the 2,000-pair query above: 28.5 s -> 7.1 s user.
This removes the index/read half of the cost; the row-carry half (binding
the list into each output row) still needs the shared representation below.

## Decision

Make the container variants of `Value` O(1) to clone by sharing their
backing storage behind `Arc` (`Value` must stay `Send`: query results cross
the query-worker thread boundary via `run_on_query_worker<T: Send>`, so `Rc`
is not an option; the atomic refcount is the price of that model):

- `Value::List(Arc<Vec<Value>>)` (or `Arc<[Value]>`)
- `Value::Map(Arc<BTreeMap<String, Value>>)`
- `Value::String(Arc<str>)` — the same argument applies to repeated string
  reads of long values

Values are immutable in the model: every mutation path already builds a new
value (`SET n = $props` replaces the map), so shared ownership cannot be
observed. Cloning becomes a refcount bump, and the evaluator's
copy-semantics call sites stay unchanged.

Narrower alternatives are explicitly rejected below.

## Alternatives considered

- **Borrowed read path in the evaluator** (`row.get_ref(n)` plus lifetimes
  threaded through `eval`): correct but invasive — every `eval` call site
  and operator changes signature, and borrows fight the mutation paths
  (`SET`, `CREATE`, deletes) that hold `&mut` graph state. Rejected as the
  primary fix; a borrow fast-path for `Subscript`/`Property` operands may
  still be added later on top.
- **Clone-on-write with copy-on-modify semantics**: same memory behaviour
  for readers, extra complexity for writers that already copy. Rejected.
- **Do nothing and document "avoid large lists"**: the datasets are real
  graphs; `collect()` over a few thousand rows is ordinary Cypher.
  Rejected.

## Consequences

- Reads of large lists/maps become O(1) to bind; the quadratic class
 disappears for the measured shapes. The pull/import path ([ADR-0155](./0155-bulk-writes-batched-unwind.md))
  benefits where it reads large property maps.
- `Arc` keeps `Value: Send + Sync`, so the query-worker boundary
  (`run_on_query_worker<T: Send>`) and the Bolt path keep working; the cost
  is one atomic increment per container clone instead of a plain refcount
  bump.
- Pointer-sized value representation grows by the refcount; the
  `Value` enum stays small (one pointer per container).
- Mutation paths that currently rely on `Value: Clone` for cheap copies of
  small values are unaffected; large-value clones become cheap everywhere.
- Regression bench: the table above (2,000 accesses at 20k elements must be
  sub-second) plus `cargo run -p cypherlite-graph-examples --example pull_rca`.

## Later references

- [ADR-0170](./0170-shared-value-payloads.md) settles the open List choice
  as `Arc<Vec<Value>>`. It extends the decision to
  `Value::Vector { coord, values: Arc<Vec<Value>> }` and records the
  migration rules for implementing it (LABS-156).
