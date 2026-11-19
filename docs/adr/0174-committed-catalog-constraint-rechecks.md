---
type: adr
area: engine
date: 2026-10-06
deciders: Oskar Hane (agreed in conversation), CypherLite maintainers
name: 0174-committed-catalog-constraint-rechecks
status: accepted
tags:
  - catalog
  - transactions
title: Re-check constraints committed mid-transaction; cancellable validation loops
REFERENCES:
  - "[[adr/0015-optimistic-wal-commits]]"
  - "[[adr/0111-engine-bounded-execution]]"
  - "[[adr/0172-incremental-constraint-validation]]"
  - "[[adr/0173-overlay-owned-redo-log-and-scratch-index]]"
  - "[[adr/0307-bolt-failure-carries-neo4j-status-codes]]"
  - "[[adr/0439-commit-time-uniqueness]]"
---

# 0174. Re-check constraints committed mid-transaction; cancellable validation loops

## Context

[ADR-0172](./0172-incremental-constraint-validation.md) made constraint validation incremental: a validation re-checks
only what the redo ops appended since the previous validation could have
broken, plus a full re-check of every constraint whose DDL is in that
slice. [ADR-0173](./0173-overlay-owned-redo-log-and-scratch-index.md) made the cursor validate-then-mark.

A review of the branch found three gaps:

1. **A constraint committed by another session is never applied to an
   open transaction's earlier writes.** An explicit transaction's
   statements read the *latest* committed schema
   (`cypherlite/cypherlite/src/connection.rs:595-598`: the schema snapshot
   is captured at dispatch, not at `BEGIN`), so `named_constraints()`
   sees the new constraint. But the recheck set is built only from the
   transaction's own redo slice, so the new constraint is checked against
   the current statement's touched entities only — never against the
   transaction's earlier, already-validated writes. Reproduced on the
   branch: `T: BEGIN; CREATE (:P {id: 1}), (:P {id: 1})`; `U: CREATE
   CONSTRAINT p_id FOR (n:P) REQUIRE n.id IS UNIQUE`; `T: CREATE
   (:Other)` succeeds and T commits two duplicates. On the base commit
   the full scan ran after every statement of the transaction, so
   `CREATE (:Other)` failed with
   ``ConstraintValidationFailed: `p_id` uniqueness`` — a behavior
   regression against main.
2. **Validation loops never check cancellation.** The full re-validation
 loop ([ADR-0172](./0172-incremental-constraint-validation.md) §5), the node uniqueness probe (§3) and the
   relationship uniqueness fallback walk (§4) iterate unbounded entity
   sets without a `cancel_tick`, so a long validation — for example a
   `CREATE CONSTRAINT` over a large label — cannot be interrupted
 ([ADR-0111](./0111-engine-bounded-execution.md)).
3. **Test gaps in `constraint_numeric_equivalence.rs`**: no end-to-end
   uniqueness cases for datetime with offset, time with offset, `-0.0`
   vs `0.0`, or points with `-0.0` coordinates, all of which follow the
 equality rules [ADR-0172](./0172-incremental-constraint-validation.md) §3 pins (`Value::eq_non_null` for the verdict,
   `GroupKey::for_property_index` for the bucket).

## Decision

1. **The scratch remembers the catalog it last validated against.**
 `RedoLog` (the owner of the validation cursor, [ADR-0173](./0173-overlay-owned-redo-log-and-scratch-index.md)) gains a
   *constraint fingerprint*: every constraint's definition and derived
   enforced label, in name order, as `named_constraints()` and
   `show_schema()` reported them. The fingerprint updates after each
   successful validation, together with the redo cursor — validate, then
   mark, unchanged. It starts unset: a scratch's first validation needs
   no diff, because its slice holds every write the scratch has made and
   each is checked against the full current catalog.
2. **Catalog drift joins the recheck set.** Each validation diffs the
   current catalog against the remembered fingerprint — O(number of
   constraints). A constraint that is new, or whose definition or
 enforced label changed, is fully re-checked ([ADR-0172](./0172-incremental-constraint-validation.md) §5) at this
   validation, exactly as if its DDL were in the slice. A dropped
   constraint needs nothing: it cannot be violated. An unchanged catalog
   and an empty slice still cost no entity inspections; the per-validation
 fixed cost becomes O(catalog size) instead of O(1) ([ADR-0172](./0172-incremental-constraint-validation.md) §8),
   which is what the pre-incremental full scan paid per statement many
   times over.
3. **Validation loops are cancellable.** The recheck loop and both
   partner searches call `ExecGraph::cancel_tick` per entity, the same
 sampled, statement-wide window every other hot loop uses ([ADR-0111](./0111-engine-bounded-execution.md)).
   A cancelled validation surfaces `Error::Execution("query cancelled")`
   and, like any failed validation, leaves the slice unvalidated.
4. **The equality pins are extended, not changed.** The new uniqueness
   cases assert the base commit's verdicts: datetimes and times compare
   by instant (`Temporal::compare` normalizes offsets away), `-0.0`
   equals `0.0` (IEEE `==` and the exact Int/Float rule), and points
   compare coordinate-wise with `==`, so `-0.0` coordinates conflict
   with `0.0` ones. `GroupKey::for_property_index` buckets each pair
   identically, so the incremental probe and the full scan agree.

## Alternatives considered

- **Pin the transaction's catalog at `BEGIN` and never see new
  constraints.** Rejected: it changes the read semantics of every
  statement of the transaction (today they read the latest committed
  schema), and it would let a transaction commit data that violates a
  constraint committed before its `COMMIT` — a wider gap than main has.
- **Recheck every constraint whenever the committed schema's version
  counter moved.** Rejected: schema versions change for index and
  graph-type reasons that cannot affect a given constraint; the
  fingerprint diff is the same O(catalog) price and re-checks only what
  actually changed.
- **Keep the empty-slice early return before any catalog read.**
  Rejected: a read-only statement of an open transaction is exactly
  where main's full scan reported a mid-transaction-committed constraint
  violation; skipping the diff would keep that regression for read-only
  statements.
- **Check cancellation only at constraint granularity.** Rejected: one
 constraint over a large label is precisely the long loop [ADR-0111](./0111-engine-bounded-execution.md)
  requires to be interruptible.

## Consequences

- The mid-transaction behavior of the base commit is restored: a
  constraint another session commits is enforced on an open transaction
  at its next statement, read-only statements included. The pre-existing
  concurrent duplicate-commit gap (both transactions commit without a
 later statement; [ADR-0015](./0015-optimistic-wal-commits.md) does no write-skew detection) is unchanged
  and out of scope.
- A validation's fixed cost is O(catalog size) even for an empty slice;
  entity inspections stay zero when neither the slice nor the catalog
 changed. [ADR-0172](./0172-incremental-constraint-validation.md) §8's "empty slice costs O(1)" becomes "O(catalog)".
- Long validations are interruptible within one cancellation sampling
  window (1024 entity ticks).
- `docs/limitations.md` states the restored mid-transaction enforcement
  alongside the remaining duplicate-commit gap.

## Later references

- [ADR-0307](./0307-bolt-failure-carries-neo4j-status-codes.md): a cancelled validation (item 3) surfaces the typed
  `Error::Cancelled` instead of `Error::Execution("query cancelled")`; the
  rendered text `query cancelled` is unchanged.
- [ADR-0439](./0439-commit-time-uniqueness.md): the pre-existing concurrent duplicate-commit gap recorded in
  Consequences is closed at the commit admission loop, not by item 1's
  mid-transaction recheck: a maintained per-constraint committed-state tuple
  map rejects the later writer with `Error::Conflict` before the WAL append,
  and the client's retry then sees the winner and reports the uniqueness
  violation. Item 1's fingerprint recheck and its semantics are unchanged.
