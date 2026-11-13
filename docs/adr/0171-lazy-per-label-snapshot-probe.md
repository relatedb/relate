# 0171. Lazy per-label probe replaces the snapshot label census

- **Status**: Accepted
- **Date**: 2026-10-06
- **Deciders**: Oskar Hane (agreed in conversation), CypherLite maintainers

## Context

`+labels` / `-labels` accounting asks two questions about a label `L` at the
write scratch's `snapshot_ts`: is `L` still live (`label_in_use`), and did
the snapshot carry it (`label_on_snapshot`). Both read
`ExecGraph::ensure_snapshot_label_n`
(`cypherlite/cypherlite-executor/src/overlay/mod.rs`), which walks
`Graph::all_node_ids_at(ts)` (every entry of the `nodes` map, a visibility
filter, then a sort) and reads `node_labels_at` per node to count **every
label in the graph**. ADR-0075 made that walk label-only; it is still
O(N) per scratch.

It runs on the first label check of a scratch: `create_node` and
`add_labels` (through `label_gain_deltas` -> `unique_unshared_labels` ->
`label_in_use`), and `remove_labels` and `delete_node` (through
`label_loss_deltas`). That covers `CREATE (n:L)`, `CREATE (n:$(row.labels))`,
MERGE-create, `SET n:L`, `REMOVE n:L` and `DELETE`. Autocommit builds a fresh
`WriteScratch` per statement (`commit_optimistic`), so every label-touching
statement pays one full census. An explicit transaction builds one scratch in
`Connection::begin` and pays once per transaction at its snapshot.
ADR-0155 lists this per-statement label cost as an open engine-side fix.

Measured 2026-10-06 (release, `pull_rca` in
`testing/cypherlite-graph-examples`):

- `CREATE (n:L) RETURN n`: 0.5 / 1.4 / 4.9 ms per op at 1k / 5k / 20k nodes
- `UNWIND` 200-row labeled create: 1 / 2 / 6 ms per statement
- unlabeled create: flat at 0.2 ms

Only labeled writes scale with the graph. The census in
`ensure_snapshot_label_n` causes the difference, and it makes loading one
element per statement O(N²).

`Graph::label_index: DashMap<u32, HashSet<NodeId>>` already holds, for each
interned label id, a superset of the nodes carrying it at any pinned
snapshot. `Graph::index_add` adds ids at commit apply, before the commit is
published. Ids leave only at GC (`index_drop_unused`), once no retained
version carries the label. Versions visible at a pinned `snapshot_ts` are
never collected (`gc_horizon`), so every node visible at the snapshot with
`L` is in the set for the scratch's lifetime. A label that was never
interned has no members. Explicit-transaction statements run without the
publication guard, so they can overlap a commit's apply. `index_add` holds
a `nodes` read guard while it takes a `label_index` write lock.

## Decision

Remove `ensure_snapshot_label_n` and `WriteScratch.snapshot_label_n`.
Label accounting answers each question with a **lazy, per-label probe**
of `Graph::label_index` that exits early. It adds no new `Graph` state.

**Definitions.** `S` is the number of nodes whose version visible at
`snapshot_ts` carries `L`. `D` is the scratch's `live_label_delta[L]`,
taken before the current operation applies. It is the net of every gain and
loss of `L` recorded in the scratch, which means the statement, or the
explicit transaction so far. A probe with threshold `k` counts visible
nodes until it has `k`, or until the set is exhausted. It returns
`min(S, k)`.

**Rules.**

1. **Only touched labels.** Only labels the statement touches are probed:
   a probe runs only for a label that the current operation gains or loses.
   No other label is enumerated or counted.
2. **Gain check** (`CREATE`, `SET n:L`, MERGE-create, dynamic labels;
   `label_in_use` with no `except` node carrying `L`): `k = 1 − D`. If
   `k ≤ 0`, `L` is in use and nothing is probed. Otherwise
   `in_use ⇔ min(S, k) ≥ k`. With `D = 0`, the case for every statement
   that has not removed `L`, the probe **stops at the first node visible at
   the snapshot**. After net removals of `L` (`D < 0`), it stops at the
   `k`-th, which is never more than the losses of `L` + 1.
3. **Loss check** (`DELETE`, `DETACH DELETE`, `REMOVE n:L`; `label_in_use`
   where the `except` node carries `L`): `k = 2 − D`. If `k ≤ 0`, nothing is
   probed. Otherwise `in_use ⇔ min(S, k) ≥ k`. The check therefore **counts
   at most (losses of that label in the statement, this one included, + 1)
   visible nodes**.
4. **`label_on_snapshot(L)`** is a probe with `k = 1`. Its callers run it
   only after a negative `in_use` probe for the same label, so the memo
   answers it without new checks.
5. **Stale ids.** A `label_index` id is counted only if its newest version
   visible at `snapshot_ts` exists and carries `L`'s label id. Check label
   ids, not label strings (ADR-0075's facet rule). Ids that are not
   visible at `snapshot_ts`, or whose visible version lacks `L`, are
   skipped and never counted. These include ids that gained `L` after the
   snapshot, ids that lost it, ids created after it, and ids waiting for
   GC. The probe ignores the scratch overlay, because the overlay's
   effects are entirely in `D`. An uninterned label has `S = 0` and needs
   no probe.
6. **Lock order.** The probe holds **at most one** of the `nodes` and
   `label_index` guards at a time. It never holds a `label_index` guard
   while acquiring a `nodes` guard, so it never reverses the
   `nodes` -> `label_index` order used by `Graph::index_add`. The probe
   works in rounds:
   1. Take a `label_index` read guard on `L`.
   2. Copy at most `c` ids out of the set (`c = 64` for the label's first
      copy in the scratch, doubled on each later copy). Record in the
      memo's `whole` whether the set length was `≤ c`, meaning the copy
      took the whole set.
   3. Drop the guard.
   4. Check each copied id under its own short `nodes` guard, holding no
      `label_index` guard.

   Never clone the whole set unless it has at most `c` ids.
7. **Memo.** Each scratch keeps one memo per probed label. It replaces
   `snapshot_label_n` and holds:
   - `checked`: ids already inspected.
   - `pending`: ids copied but not yet checked, kept in copy order.
   - `visible`: the count of checked ids that passed the stale-id check.
   - `whole`: whether the latest copy took the whole set.
   - `complete`: whether the whole set has been checked.

   A probe proceeds in this order:
   1. If `visible ≥ k`, answer positive.
   2. Otherwise, if `complete`, answer from `visible`.
   3. Otherwise, check ids from `pending` in order, and answer positive as
      soon as `visible = k`.
   4. When `pending` is empty: if `whole`, set `complete` and answer from
      `visible`. Otherwise copy again (same lock order), make the ids not
      in `checked` the new `pending`, and continue at step 3.

   `complete` is set only after every id of a whole-set copy has been
   checked. Only then may a probe conclude "fewer than `k`". Each id is
   checked at most once per scratch. Visibility at a fixed, pinned
   `snapshot_ts` never changes, so the memo is never invalidated. The
   memo records facts about the snapshot, not the scratch's writes, so
   `scratch_snapshot` / `restore_scratch` (CALL … IN TRANSACTIONS) share
   it rather than deep-copying it. A restore keeps what was learned.
8. **Explicit transactions** probe at the **transaction's `snapshot_ts`**
   with the transaction's scratch, which holds its `D` and its memo.
   Concurrent commits after `begin` can add only ids that the stale-id
   check skips, because an id visible with `L` at the snapshot is already
   in the set.
   GC cannot drop ids of versions visible at the pinned snapshot. Let
   `need` be 1 for a gain and 2 for a loss. Each answer `min(S, k) ≥ k`
   with `k = need − D` is equivalent to the census's `S + D ≥ need`, and
   `min(S, 1) ≥ 1 ⇔ S ≥ 1`. So the `+labels` / `-labels` values are
   **identical to what the census produced**. The same holds for
   autocommit statements.

## Alternatives considered

- **Keep the census.** It is correct and needs no new code, but it costs
  O(N) plus a sort per label-touching autocommit statement. It is the
  measured scaling quoted in Context, and it makes per-element loads
  O(N²). Rejected: every labeled write pays for the whole graph.
- **Maintain committed per-label counts in `Graph`** (the global
  label -> count index ADR-0075 rejected). One committed count cannot answer
  "at `snapshot_ts`" for an explicit transaction whose snapshot predates
  later commits. Answering that needs counts versioned by commit ts, pruned
  at the GC horizon, updated on every apply and abort, and rebuilt on
  checkpoint restore. Rejected: it adds new global state to every commit
  path to answer what the existing `label_index` answers with no new state,
  and ADR-0075's reason still holds.
- **Cache the census across statements** (keyed by `commit_ts`, invalidated
  on commit). Every commit invalidates it, and in the per-element load
  every statement is a commit, so the first statement after each commit
  still pays O(N). Rejected: it does not fix the measured case, and
  invalidation must track every commit's label changes (ADR-0075's
  reason).
- **Use `label_index` set sizes as counts.** This is O(1) per label, but the
  sets are supersets: they hold stale, post-snapshot and relabeled ids.
  Rejected: the counts are wrong under MVCC, and re-checking every member
  is O(|L|) again.
- **Clone the label's set and filter it**, as `lookup_nodes_at` does.
  Rejected: O(|L|) per statement for a populated label. A labeled create
  would still scale with the label's size.
- **Iterate the set in place while checking visibility.** Rejected: this
  holds a `label_index` guard while acquiring `nodes` guards. That is the
  reverse of `index_add`'s order, and with writer-preferring shard locks
  it can deadlock against a concurrent apply.
- **Approximate or drop `+labels` / `-labels`.** Rejected: the counters are
  part of the result contract on every surface, and TCK scenarios assert
  them.

## Consequences

- `CREATE (n:L)` on a populated label checks O(1) snapshot nodes: the first
  visible id, plus any stale ids ahead of it. Repeated gains of `L` in one
  statement probe once, because `D > 0` afterward. A new or uninterned label
  costs O(1). A label whose set holds only stale or post-snapshot ids costs
  that set's size until GC prunes it. The labeled numbers in Context are
  expected to go flat; the implementing change re-measures them with
  `pull_rca`.
- Deleting or relabeling all of `L` is linear. Thanks to the memo, at most
  `|label_index[L]|` ids are checked per label per scratch. The per-scratch
  worst case is the sum of the touched labels' set sizes, a small multiple
  of the old census only for statements that delete or relabel most of a
  multi-label graph.
- ADR-0075's census, the subject of its Decision, is gone.
  `Graph::node_labels_at` stays because other callers use it. ADR-0075's
  rejected alternatives stay rejected: there is no global count and no
  cross-statement cache. ADR-0155's per-statement label cost item is
  resolved.
- There is no change to public API, result metadata, WAL, checkpoint or
  stats semantics.
- **Tests.**
  - **Counter.** Add a test-only thread-local counter of snapshot nodes
    inspected by label accounting, following the `HOP_EXPANDS` pattern in
    `cypherlite/cypherlite-executor/src/exec/mod.rs`. It increments once
    per id whose snapshot version label accounting checks and has a
    `#[cfg(test)]` `take_…` helper.
  - **Flat cost.** Assert that the count for `CREATE (n:L)` with `L`
    populated is **equal at two graph sizes** (e.g. 1k and 10k nodes).
  - **Linear cost.** Assert that `MATCH (n:L) DELETE n` checks at most
    `|L|` ids.
  - **Parity with the census.** Cover new label, existing label, deleting
    the last and a non-last node, `REMOVE` of the last carrier, `SET` of a
    new label, delete-then-create of `L` in one statement, and an explicit
    transaction with a concurrent commit that adds and removes `L` after
    `begin`. Every case must keep its existing `+labels` / `-labels` values.
