---
type: adr
area: engine
claim: 97a46d752ea5d02f10822749d17c689c286df0ec
name: 0355-fxhash-core-maps-and-interned-domain-reads
status: accepted
tags:
  - perf
title: FxHash core maps and interned-domain reads on the traversal hot path
---

# ADR-0355: FxHash core maps and interned-domain reads on the traversal hot path

## Context

The q19 investigation (LABS-337, LDBC SF0.1: ~300ms p50 for a query whose
true row work is ~120 rows) profiled with the in-process sampler
(`make query-latency-profile QID=19`) split the `Expand` cost into three
engine-side terms, each a per-traversal-step overhead the query's row
count does not need:

1. **Allocation churn** — the agg-fold path walk allocated a fresh
   `bindings.to_vec()` per hop and a fresh neighbor `Vec` per start node
   (allocator self-time 52%, `dealloc` alone 49.9%).
2. **Candidate-check string round-trips** — `node_ok` materialized every
   candidate's labels as a `Vec<String>` (`node_stub` de-interning) even
   for patterns whose label conjunction is already served by
   `node_candidates`, and read properties by re-interning the key string
   per candidate.
3. **SipHash probing on the core maps** — the graph's stores
   (`nodes`, `rels`, `out_adj`, `in_adj`, the label index, and the pair
   indexes) are `DashMap`s on the default hasher, so every hop, candidate
   check, and property read pays SipHash + shard lock + probe
   (`_get<Vec<Ver<LiveNode>>>` 16.2% self after (1) was fixed).

The structural fix for (3) — dense vector stores indexed by the integer
ids — is LABS-358, its own decision; this ADR records the interim
engine-side decisions that shipped now.

## Decision

1. The traversal hot path's **candidate checks stop materializing
   strings**: `node_ok` checks static label conjunctions (the common
   case — the planner synthesizes `label_expr` even for plain
   `MATCH (n:Person)`) label-by-label via `node_has_label`, paying one
   interner lookup per (candidate, label) with no `Vec<String>`;
   genuinely dynamic label expressions take the `node_labels` +
   `label_expr_matches` path, whose `None` is its own visibility read;
   a pattern with no `label_expr` checks bare visibility via
   `node_exists`. Properties read via the string-keyed `node_prop` —
   one interner lookup per (candidate, key), the same count any
   wrapper of it pays, and the scratch (uncommitted) map it reads is
   string-keyed by construction. A kid-keyed read API
   (`intern_key`/`node_prop_kid`) was built in an earlier round of
   this change and removed in review: it re-spelled `node_prop`'s
   interning at the call site and bought nothing without a per-pattern
   hoist the profile never justified.
2. The **core graph maps use FxHash** (`rustc-hash`, `CoreMap<K, V>` =
   `DashMap<K, V, BuildHasherDefault<FxHasher>>`): every map in the graph
   module is keyed by dense integers or integer pairs, where SipHash's
   defense buys nothing. The procedures map stays on the default hasher
   (string keys, cold path).
3. The agg-fold walk **reuses buffers**: bindings are extended by
   save/restore of the two slots rather than a per-hop `to_vec()`, and
   the neighbor buffer is one `Vec` reused across the start nodes.

## Alternatives considered

- **Do nothing until LABS-358's dense-vector store lands** — rejected:
   LABS-358 is a structural change with its own ADR and MVCC/checkpoint
   work; the profiled costs here are removable without it, and each
   removal shrinks the profile to make LABS-358's remaining win
   measurable.
- **Keep SipHash on the core maps (defense-in-depth on map keys)** —
   rejected: the keys are engine-generated dense integers
   (`NodeId`/`RelId`/interned ids), not attacker-controlled strings; the
   label-probe procedures map (string-keyed, reachable from Cypher
   procedure calls) stays on SipHash.
- **A per-pattern pre-interned check context (memoized label/key ids per
   `NodePattern`)** — deferred: the per-check interning that remains is
   one hash lookup per pattern key per candidate set, well under the
   profile's noise floor once (2) removes the per-probe SipHash cost.

## Consequences

- q19 at SF0.1 (interleaved A/B, 3 runs each): mean p50 ~297ms (main) →
  ~252ms (this change) — ~15%, on top of the churn fix's ~5%; the
  profiled allocator share dropped 52% → 10% and `dealloc` left the top
  list.
- All map keys in the graph module are integer-typed; a future key that
  is attacker-controlled must not use `CoreMap`.
- The remaining dominant term is the DashMap probe itself (26% group),
  which LABS-358 removes structurally.
- `rustc-hash` joins the workspace deps (no transitive tree; R6-light).
