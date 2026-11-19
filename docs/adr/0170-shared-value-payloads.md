---
type: adr
area: engine
date: 2026-10-06
deciders: CypherLite maintainers
name: 0170-shared-value-payloads
status: accepted
tags:
  - perf
  - values
title: "Shared `Value` payloads: `Arc` for String, List, Map and Vector"
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0127-store-version-compatibility]]"
  - "[[adr/0144-graph-examples-differential-suite]]"
  - "[[adr/0154-value-clone-cost]]"
---

# 0170. Shared `Value` payloads: `Arc` for String, List, Map and Vector

## Context

[ADR-0154](./0154-value-clone-cost.md) (Accepted) found, through the graph-examples differential suite
([ADR-0144](./0144-graph-examples-differential-suite.md)), that the executor deep-clones a bound value on every variable
read, and decided to back `Value::List`, `Value::Map` and `Value::String`
with `Arc`. It left `Arc<Vec<Value>>` versus `Arc<[Value]>` open and did
not cover `Value::Vector`.

The by-reference operand read for `Expr::Subscript` and `Expr::Property`
(`eval_operand`, #161) removed the index half of the cost. The row-carry
half remains: `cypherlite-executor/src/eval/mod.rs` still evaluates
`Expr::Variable(n) => Ok(row.get(n).cloned().unwrap_or(Value::Null))`
over `Row = IndexMap<String, Value>`, so every projection that carries a
large binding into a new row copies it. LABS-156 reproduces this on
v0.11.4 at N = 20,000:

```cypher
UNWIND range(1, N) AS i CREATE (n:N {i: i})
WITH collect(n) AS ns
UNWIND range(0, 1999) AS k
WITH k, ns[k] AS a, ns[(k + 1) % N] AS b
RETURN count(*)
```

It takes 8.6 s user + 17.5 s system; creating and collecting alone takes
0.11 s.

`Value::Vector { coord, values: Vec<Value> }` has the same shape as a list.
Embeddings are hundreds to thousands of elements and are read once per row
in similarity queries, so they pay the same clone per read.

Implementing [ADR-0154](./0154-value-clone-cost.md) touches every crate that constructs or destructures
these variants. It needs the open representation choice settled, the
Vector scope decided, and one set of migration rules so the call sites stay
O(1) after the change.

## Decision

1. The four payloads are shared behind `std::sync::Arc` (`Value` stays
 `Send + Sync`, as [ADR-0154](./0154-value-clone-cost.md) requires):

   - `Value::String(Arc<str>)`
   - `Value::List(Arc<Vec<Value>>)`
   - `Value::Map(Arc<BTreeMap<String, Value>>)`
   - `Value::Vector { coord: VectorCoordKind, values: Arc<Vec<Value>> }`

2. List and Vector use `Arc<Vec<Value>>`, not `Arc<[Value]>`. This settles
 the choice [ADR-0154](./0154-value-clone-cost.md) left open.
3. Vector extends [ADR-0154](./0154-value-clone-cost.md). The rationale is the same: embeddings that are
   read once per row hit the same clone-per-read cost as lists.
4. No new public constructor or accessor functions are added to `Value`.
   Callers construct with `Arc::new(..)` or `.into()`
   (`Value::List(vec![..].into())`, `Value::String("x".into())`) and
   consume with `Arc::unwrap_or_clone`. Existing methods keep their
   signatures (for example, `as_string() -> Option<&str>`).
5. The payload types of `cypherlite::Value` (the re-export of
   `cypherlite_core::Value`) change. This is breaking for Rust callers that
   construct or destructure these four variants, and `docs/api/rust/spec.md`
   `## Value` documents the new payloads in the implementing change. The
   C ABI, Bolt, WAL/JSON/Parquet bytes (their serializers take `&Value`) and
   `cypherlite_model::CypherValue` are unchanged. So are
 `Value::logical_bytes` results ([ADR-0042](./0042-graph-memory-limit.md)) and `Debug` output (`Arc`
   delegates to its contents).
6. Every implementing change follows these migration rules:
   - **Construct**: build an owned `Vec`/`BTreeMap`/`String`, then wrap
     it with `Arc::new(..)` or `.into()`.
   - **Read**: borrow through deref (`xs.iter()`, `xs.len()`, `m.get(k)`,
     `&**s`, `s.as_ref()`). Pass `&[Value]`, `&BTreeMap<String, Value>`
     or `&str` to helpers. Cloning a `Value` or a payload is a refcount
     increment.
   - **Consume owned**: use `Arc::unwrap_or_clone`, which moves the
     payload when the Arc is unique and copies it once otherwise. Never
     call `.to_vec()`, `(**x).clone()`, `.as_ref().clone()` or
     `.iter().cloned().collect()` on an owned payload. `Arc<str>` has no
     `unwrap_or_clone`, so borrow `&str` and call `String::from(&*s)` only
     where an owned `String` is required.
   - **Mutate**: call `Arc::make_mut` only on a freshly built or provably
     unique Arc. Never call it inside a per-element loop over a value read
     from a row, a parameter or the graph, because each call on a shared
     Arc copies the whole payload.
   - **Build**: builders (`collect`, list and pattern comprehensions,
     decoders, `Expr::List`/`Expr::Map`) accumulate an owned
     `Vec`/`BTreeMap`/`String` and wrap it once, after the loop.
   - **Compare**: use the existing `Value` methods and derived
     `PartialEq` (`Arc<T>: PartialEq` compares contents). Compare strings
     by contents (`&**s == "x"`), never with `Arc::ptr_eq`.
7. Out of scope: `Node`, `Relationship` and `Path` payloads stay owned.
   `CypherValue` is unchanged. Copies inherent to building a new value
   (list concatenation, `reduce` accumulators, string concatenation) stay
   O(n).

## Alternatives considered

- **`Arc<[Value]>` for List and Vector**: saves one indirection per
  element access. However, `Arc<[T]>::from(Vec<T>)` allocates again and
  copies the elements into the new allocation on every construction
  (`collect`, comprehensions, decoders, list literals). `Arc<[T]>` also
  has no `Arc::make_mut`-then-push path and no `Arc::unwrap_or_clone`
  path to a `Vec`, so every consumer that needs an owned `Vec` copies the
  slice. Rejected because one extra indirection costs less than a copy on
  every construction and every consume.
- **`Arc<String>` for String, to match `Arc<Vec<Value>>`**: would allow
  `Arc::unwrap_or_clone` to a `String`. Strings are compared, matched and
  displayed far more often than they are taken apart. No engine path grows
  a string in place, because concatenation builds a new value. Converting
  `String` to `Arc<str>` copies the bytes once, with no per-element clones.
 Rejected because `Arc<str>` ([ADR-0154](./0154-value-clone-cost.md)'s choice) saves an indirection on
  every string read.
- **Keep Vector owned (`Vec<Value>`)**: smaller diff that stays within the
 scope of [ADR-0154](./0154-value-clone-cost.md). Embeddings are the largest values that are routinely
  read per row, and keeping Vector owned would leave a different
  representation from List for the same element type. Rejected because it
  keeps exactly the clone-per-read cost this ADR removes, for the values
  where it is largest.
- **`Rc` instead of `Arc`**: a non-atomic refcount is cheaper. `Value`
  must be `Send`, though: query results cross
  `run_on_query_worker<T: Send>` (`cypherlite/cypherlite/src/connection.rs`).
 Rejected, as in [ADR-0154](./0154-value-clone-cost.md), because `Rc` makes `Value` `!Send`.
- **Constructor and accessor helpers** (`Value::list(Vec<Value>)`,
  `Value::string(..)`, `into_list() -> Option<Vec<Value>>`): would hide
  `Arc` from Rust callers and soften the break. Each helper is permanent
  public API that needs spec, description and an example (AGENTS.md §2),
  and duplicates what `Arc::new`, `.into()` and `Arc::unwrap_or_clone`
  already express. Helpers would also hide whether a consume moves or
  copies, which is the cost this ADR targets. Rejected because they add
  surface without adding capability.
- **Also share `Node`/`Relationship`/`Path` payloads**: entity values carry
  property maps and label lists and are cloned per read too. A later ADR
  can take this up with its own measurements. Rejected for this change
  because these values are built from the executor's graph store, so
  sharing them changes more than `Value`, and LABS-156 does not measure
  them.

## Consequences

- Variable reads and row carries of String, List, Map and Vector values
  become refcount increments, which removes the quadratic class
 The regression bar from [ADR-0154](./0154-value-clone-cost.md) still applies (2,000 accesses
  at 20k elements must be sub-second). The implementing change adds the
  LABS-156 query as a regression test.
- Rust callers that construct or destructure these variants must migrate
  using the rules in Decision §6. This includes the in-repo surface crates
  that use `cypherlite::Value` directly, which migrate in the same change.
  Users of the Python, JavaScript, Java, Go, WASM and Bolt surfaces see no
  change, because those surfaces expose `CypherValue` or serialized forms.
- Persisted bytes are identical, so no snapshot or WAL format version
 changes and no store migration ([ADR-0127](./0127-store-version-compatibility.md)) runs.
- `logical_bytes` is unchanged. Memory-limit accounting ([ADR-0042](./0042-graph-memory-limit.md)) and
  baseline logical bytes do not move. Accounting stays logical: a payload
  shared by several rows is counted once per reference.
- No payload grows, so `size_of::<Value>()` does not grow. Each clone of
  these variants costs one atomic increment.
- `Arc::make_mut` on a shared payload copies it in full. The Mutate rule
  keeps that copy out of per-element loops. Reviews of implementing
  changes check for it, together with the banned consume forms in the
  Consume owned rule.
- `Node`/`Relationship`/`Path` clones, `Value` to `CypherValue` conversion
  at the result boundary, and copies inherent to building a new value keep
  their current cost.
