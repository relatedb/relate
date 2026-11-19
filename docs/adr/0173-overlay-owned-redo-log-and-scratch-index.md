---
type: adr
area: engine
date: 2026-10-06
deciders: CypherLite maintainers (review of the [ADR-0171](./0171-lazy-per-label-snapshot-probe.md)/[ADR-0172](./0172-incremental-constraint-validation.md) branch)
name: 0173-overlay-owned-redo-log-and-scratch-index
status: proposed
tags:
  - index
  - write-path
title: Overlay-owned redo log and scratch equality index
REFERENCES:
  - "[[adr/0172-incremental-constraint-validation]]"
SUPERSEDES:
  - "[[adr/0172-incremental-constraint-validation]]"
---

# 0173. Overlay-owned redo log and scratch equality index

## Context

[ADR-0172](./0172-incremental-constraint-validation.md) made constraint validation incremental. Its implementation left
four pieces of write-overlay state whose invariants are kept by convention
across several functions:

1. **The scratch equality index.** `WriteScratch.prop_ix` is the lazy
   `(label, key) → GroupKey → ids` overlay the uniqueness probe and
 `ExecGraph::lookup_nodes` read ([ADR-0172](./0172-incremental-constraint-validation.md) §3). Three hooks keep it
   current: one each in `create_node`, `set_property_node` and `add_labels`.
   The hooks differ on whether they post to the `""` any-label bucket, and
   they share a free function over a raw nested `HashMap` to get around the
 borrow checker. [ADR-0172](./0172-incremental-constraint-validation.md) rejected hooks in every write primitive for the
   touched set, because a primitive that forgets its hook silently skips
   validation. The uniqueness probe trusts this index in the same way: a
   missing posting is a missed conflict.
2. **The redo log.** `pending_redo`, `catalog_redo` and `validated_redo` are
   three loose `WriteScratch` fields. Their coupled invariants are maintained
   in `overlay/mod.rs` and mutated from `overlay/validation.rs`, and the
   cursor read clamps with `.min(len)`, hiding the invariant
   `validated_redo <= pending_redo.len()`.
3. **Drain-on-entry.** [ADR-0172](./0172-incremental-constraint-validation.md) §1 advances the validation cursor *before*
   the slice is checked. Today's callers are safe: `execute` propagates the
   error and the transaction fails or is discarded, and `CALL { … } IN
   TRANSACTIONS` restores the scratch. A future caller that swallows the
   error would leave the violating ops marked validated, and the next
   validation would accept them.
4. **`all_node_ids` / `all_rel_ids`.** [ADR-0172](./0172-incremental-constraint-validation.md) §8 merges the scratch ids
   into the snapshot ids with a `binary_search` over the prefix of the
   snapshot ids. That relies on undocumented sortedness of the base
   iterators and re-checks every id's existence.

`RedoOp::is_catalog` is also a third exhaustive listing of every `RedoOp`
variant, although `RedoOp::write_target` already classifies catalog ops.

[ADR-0172](./0172-incremental-constraint-validation.md) §2 ("Unknown tags") keeps a stored kind tag that
`ConstraintType::from_str` rejects as a constraint that is never required
and never unique, checked for its property type only. Such a constraint
enforces less than its definition says, and nothing reports it: the
validation fails open for the kind, not closed.

## Decision

1. **`ScratchPropIndex`** (`overlay/prop_ix.rs`) owns the scratch equality
   index. It has a `seek`, which builds a `(label, key)` pair on first use
   from the scratch nodes, and post methods. The any-label bucket is one
   named constant that both use. Buckets are keyed `key → label`, so a post
   looks up `&str` keys and allocates no `String`.
2. **Posting is driven by the redo funnel.** `WriteScratch::push_redo` is
   the only caller of the posting. It matches `RedoOp` exhaustively, with
   no wildcard arm:
   - `CreateNode` posts each property under the any-label bucket and each of
     its labels. It reads the op payload, because `create_node` pushes the
     op before it inserts the node.
   - A non-null `SetNodeProp` posts the value under the any-label bucket and
     each of the node's current labels.
   - `AddLabels` posts each of the node's properties under each added label.
     A label does not change which values the node carries, so its
     any-label pairings are unchanged.
   - Every other op posts nothing. The index stays additions-only, a
 superset that every caller re-verifies ([ADR-0172](./0172-incremental-constraint-validation.md) §3).

   A new `RedoOp` variant fails to compile in that match. The write
   primitives contain no index calls. A primitive that skipped
   `push_redo` would lose durability, so the index cannot fall behind
   the log.
3. **`RedoLog`** (`overlay/redo_log.rs`) owns the ops, the catalog
   positions and the validation cursor. `WriteScratch` holds one `RedoLog`.
   It exposes `push`, `take`, `catalog_ops`, `unvalidated` (the
   unvalidated slice and the position just past it) and
   `mark_validated(end)`. The cursor invariant is a `debug_assert`, not a
   clamp.
4. **Validate, then mark.** This replaces [ADR-0172](./0172-incremental-constraint-validation.md) §1's drain-on-entry. A
   validation reads the unvalidated slice and its end position, classifies
   and checks it, and calls `mark_validated(end)` only after the check
   returned `Ok`. A failed validation leaves its slice unvalidated, so
   validating the same scratch again fails again.
5. **`all_node_ids` / `all_rel_ids`** are built as the snapshot ids, minus
   the scratch's deleted ids, plus `created_nodes` / `created_rels`, then
 sorted and deduplicated. This replaces [ADR-0172](./0172-incremental-constraint-validation.md) §8's `binary_search`
   merge. The invariant behind it holds in the code:
   - a copy-on-write entry is read from the snapshot first, so it is already
     a snapshot id;
   - `created_*` and the scratch maps gain an id together in `create_*`;
   - a delete removes the id from `created_*`, the scratch map and the
     result.

   The two sets meet only if an allocator hands out a committed id;
   `dedup` keeps that case to one entry.
6. **`RedoOp::is_catalog`** is `write_target().is_catalog()`. The new
   `WriteTarget::is_catalog` is exhaustive, so `RedoOp`'s variants are
   classified in one place (`write_target`).
7. **An unknown kind tag fails validation.** A validation that checks a
   constraint whose stored kind tag `ConstraintType::from_str` rejects
   fails with ``ConstraintValidationFailed: `{name}` `` followed by the
   parser's reason, an `Error::semantic`, the way an unparseable stored
 property type already fails ([ADR-0172](./0172-incremental-constraint-validation.md) §6). The tag is parsed once per
   validation, before any entity is checked, so every write statement
 fails while such a constraint exists. This replaces [ADR-0172](./0172-incremental-constraint-validation.md) §2's
   "Unknown tags" rule.

## Alternatives considered

- **Keep the per-primitive hooks and make them agree on the any-label
 bucket.** Rejected: it keeps the drift risk [ADR-0172](./0172-incremental-constraint-validation.md) rejected for the
  touched set. A future primitive, or a change to an existing one, that
  forgets its hook silently weakens the uniqueness probe.
- **Rebuild a pair from the scratch nodes on every seek.** It needs no
  posting at all. Rejected: it costs O(scratch) per probe, the per-statement
 quadratic term [ADR-0172](./0172-incremental-constraint-validation.md) removed.
- **Keep drain-on-entry.** Rejected: it is correct only while every caller
  propagates the error or restores the scratch, and nothing enforces that.
- **Keep the `binary_search` merge and document the sortedness.** Rejected:
  the created set already is the set of scratch-only ids, so the merge and
  its per-id existence checks are unnecessary.
- **Skip a constraint with an unknown kind tag, or check only its property
 type ([ADR-0172](./0172-incremental-constraint-validation.md) §2).** Rejected: either way a definition is silently
  enforced less than it says. `CREATE CONSTRAINT` and graph-type
  derivation write only known tags, so the failure can only come from a
  damaged or foreign catalog, which should surface.

## Consequences

- No accept/reject decision or error text changes for today's callers:
  both already propagate or restore on failure. The one observable
  difference is a second validation of a scratch whose last validation
  failed: it now fails again, where drain-on-entry returned `Ok`. A unit
  test pins it.
- `CALL { … } IN TRANSACTIONS` still restores the scratch, cursor included,
  on a failed batch, and still hands unvalidated outer writes to the next
 validation ([ADR-0172](./0172-incremental-constraint-validation.md) §1, §7).
- [ADR-0172](./0172-incremental-constraint-validation.md) §1 ("Cursor"), the scratch side of §3, and the
  `all_node_ids` bullet of §8 are superseded in those details. Their
  algorithms, costs and guarantees are unchanged.
- Catalog views of the overlay (`show_schema`, `named_indexes`,
  `named_constraints` and the named create/drop helpers) move to
  `overlay/catalog.rs`, next to the log they replay.
- A catalog holding an unknown kind tag now fails every write statement
  with `ConstraintValidationFailed` until the constraint is dropped, where
 [ADR-0172](./0172-incremental-constraint-validation.md) §2 accepted writes that broke the kind's rule. Every catalog
  written by `CREATE CONSTRAINT` or `ALTER CURRENT GRAPH TYPE` is
  unaffected; a unit test pins the failure.
