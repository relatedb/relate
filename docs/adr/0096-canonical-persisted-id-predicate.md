---
type: adr
area: engine
date: 2026-09-24
deciders: CypherLite maintainers
name: 0096-canonical-persisted-id-predicate
status: proposed
tags:
  - storage
title: One canonical persisted-ID predicate
REFERENCES:
  - "[[adr/0086-bounded-persistence-input]]"
  - "[[adr/0094-preflight-wal-acceptance]]"
---

# 0096. One canonical persisted-ID predicate

## Context

[ADR-0086](./0086-bounded-persistence-input.md) (5) makes persisted node and relationship IDs (including relationship
endpoints) an unsupported-sentinel boundary: an ID `== i64::MAX` is rejected
with a typed `Error::Storage`, and a negative ID is rejected before it can
seed a next-write counter or be installed into the graph. [ADR-0094](./0094-preflight-wal-acceptance.md) (1) requires
that "exhaustive `RedoOp` ID preflight remains a single RedoOp-owned rule
shared by WAL and graph (the follow-up to [ADR-0086](./0086-bounded-persistence-input.md))". That follow-up is this
decision.

The bound `0..i64::MAX` (reject `id < 0`, reject `id == i64::MAX`) is
currently implemented **five separate times** in two crates:

1. `crates/cypherlite-executor/src/redo/mod.rs` — `RedoOp::validate_ids`, the
   inner `check(id, what)` closure, rejecting `id < 0 || id == i64::MAX` and
   formatting `"{what} ID {id} is outside 0..i64::MAX"`.
2. `crates/cypherlite-executor/src/graph/mod.rs` — `fn checked_persisted_id`,
   rejecting `id < 0` and `id == i64::MAX` separately and returning
   `Ok(id + 1)` so the caller can advance the next-ID allocator.
3. `crates/cypherlite-storage/src/wal.rs` — `pub(crate) fn
   validate_persisted_id`, a storage-local copy used only by snapshot
   pull-identity rows. Its own doc comment defers collapsing it "only behind an
   ADR-first API change"; this ADR is that change.
4. `crates/cypherlite-storage/src/codec/parquet.rs` — `fn valid_id`, which
   rejects only the `id == i64::MAX` sentinel before/after decode.
5. `crates/cypherlite-storage/src/codec/json.rs` — the inline `.any()` chains
   in `graph_from_data` that reject `id == i64::MAX` for nodes, relationship
   IDs, and relationship endpoints (plus the `next_node_id < 0` /
   `next_rel_id < 0` guards).

The two executor copies can drift from the two codec copies and the WAL copy;
the parquet/json copies check only the sentinel while the executor/WAL copies
also reject negatives. A single source of truth is needed so the WAL, graph,
and both snapshot codecs cannot disagree about what "persisted ID" means.

## Decision

Define **one** canonical predicate for persisted local IDs in
**`cypherlite-core`**, next to `NodeId` and `RelId`:

```rust
// cypherlite-core, beside `pub type NodeId = i64;` / `pub type RelId = i64;`
pub fn validate_persisted_id(id: i64, what: &str) -> cypherlite_core::Result<()>;
```

It rejects `id < 0 || id == i64::MAX` and formats one canonical storage error
carrying the caller-supplied `what` label; the label preserves per-site
context and the range text is fixed in one place. All five sites above are
replaced by it:

1. `redo/mod.rs` `validate_ids`'s inner `check` calls
   `cypherlite_core::validate_persisted_id`, so `Wal::validate_redo_ids` and
   replay inherit the same rule.
2. `graph/mod.rs` `checked_persisted_id` becomes a thin wrapper: it calls the
   canonical predicate, then returns `Ok(id + 1)` for its callers' `fetch_max`
   allocator advance. The predicate stays a boolean range check; the `+1`
   allocator step is graph-local behavior and is not folded into the shared
   predicate.
3. `wal.rs`'s `pub(crate) validate_persisted_id` is deleted; the snapshot
   pull-identity sites call the core predicate directly.
4. `codec/parquet.rs`'s `valid_id` is deleted in favour of the core predicate
   at its write and read call sites.
5. `codec/json.rs`'s inline `.any()` sentinel checks are replaced by the core
   predicate (or a thin bulk helper that calls it per ID); the negative
   next-counter guards remain their own next-ID check.

**Why `cypherlite-core` is the home.** Both `cypherlite-storage` and
`cypherlite-executor` already depend on `cypherlite-core`; `cypherlite-core`
depends on neither. `NodeId`/`RelId` are defined in core, so the range rule
that constrains them belongs beside its types. `cypherlite-storage` gets a
single-ID predicate without widening the executor's redo-op API, and future
non-executor persistence code can call it without importing the query layer.

The `wal.rs` comment's "collapse them only behind an ADR-first API change"
condition is satisfied by this ADR; implementation may proceed in a follow-up
change.

## Alternatives considered

- **Keep storage-crate private copies (leave `wal.rs::validate_persisted_id`
  and `codec/parquet.rs::valid_id` as-is).** Rejected: it is exactly the
  duplication this decision removes; a future tightening of the bound (or a
  change to the sentinel semantics) would have to be made in every copy
  independently, and the parquet/json copies would keep checking only the
  sentinel while the executor/WAL copies also reject negatives.
- **Keep the op-shaped executor-only API (`RedoOp::validate_ids`) as the only
  home.** Rejected: it validates an entire redo op, not a single ID, so the
  snapshot pull-identity rows, `codec/parquet.rs`, and `codec/json.rs` cannot
  use it; forcing non-op callers through a `RedoOp` would either duplicate the
  check or fabricate ops. It also couples the storage codecs to the executor's
  redo variant set.
- **Home the predicate in `cypherlite-executor`.** Rejected: storage already
  depends on executor, so it would work, but it places a primitive `i64` range
  rule on the graph/query layer and requires the graph crate's redo API to
  expose it publicly; core already owns `NodeId`/`RelId` and is the lower
  common dependency both crates share.
- **Home it in `cypherlite-storage` and have the executor depend on storage.**
  Rejected: `cypherlite-storage` already depends on `cypherlite-executor`;
  reversing that edge creates a dependency cycle.
- **Have the predicate return the advanced next counter (`id + 1`).** Rejected:
  the allocator advance is a `Graph` concern, and snapshot/WAL callers want a
  pure acceptance check; returning a counter would couple the shared predicate
  to one caller's `fetch_max` protocol.
- **Encode the rule as a macro or a per-ID newtype implementer.** Rejected:
  `NodeId`/`RelId` are plain `i64` aliases, and a macro/newtype would spread
  the bound across call sites and generated code instead of keeping it in one
  function.

## Consequences

- `cypherlite-core` gains one small public function and its documentation; the
  five duplicated range checks are deleted as their call sites route through
  it. The public surface of `cypherlite-executor` is not widened for this
  predicate.
- Snapshot Parquet/JSON decode and encode now reject negative persisted IDs as
  well as the `i64::MAX` sentinel through the same predicate, matching the
  executor and WAL rule; accepted data is unchanged because negative IDs were
  already rejected downstream at `load_checkpoint_node` /
  `load_checkpoint_rel`.
- Error strings for the affected sites converge on the canonical
  `{what} ID {id} is outside 0..i64::MAX` wording. Any tests that assert the
  previous site-specific wording (`unsupported negative {what} ID`,
  `i64::MAX is reserved`, `Parquet shard: unsupported {what} sentinel`) are
  updated in the implementing change.
- A later change to the accepted ID range or sentinel is one edit in
  `cypherlite-core`; the WAL, graph, and both codecs cannot drift.
- This ADR changes no behavior on its own; it records the decision required
  before the mechanical replacement lands.
