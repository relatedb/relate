---
type: adr
area: engine
claim: 5798c270ad9a66d0453660cb9673e523348ab0e9
name: 0439-commit-time-uniqueness
status: proposed
tags:
  - catalog
  - write-path
title: Commit-time uniqueness admission
REFERENCES:
  - "[[adr/0015-optimistic-wal-commits]]"
  - "[[adr/0017-client-conflict]]"
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0172-incremental-constraint-validation]]"
  - "[[adr/0174-committed-catalog-constraint-rechecks]]"
  - "[[adr/0184-composite-key-smallest-bucket-probe]]"
  - "[[adr/0185-relationship-equality-postings]]"
---

# ADR-0439: Commit-time uniqueness admission

## Context

[ADR-0172](./0172-incremental-constraint-validation.md) made constraint validation incremental: each statement validates the entities its redo slice touched against its pinned snapshot plus its own scratch. [ADR-0174](./0174-committed-catalog-constraint-rechecks.md) closed the mid-transaction catalog gap. Both enforce uniqueness against the state a statement can see.

The concurrent case is still open. Two transactions can each `CREATE` the same value under a `unique` or `key` constraint and both commit: the statement-time validator compares each against its own pin, and the commit admission loop checks element ids and the catalog only (`Graph::write_set_conflicts`). [ADR-0015](./0015-optimistic-wal-commits.md) deliberately does no write-skew detection, [ADR-0172](./0172-incremental-constraint-validation.md) and [ADR-0174](./0174-committed-catalog-constraint-rechecks.md) record the duplicate-commit gap as out of scope, and `docs/limitations.md` states it. Finding 2 makes closing it this change.

The admission loop is the seam. `flush_group` (`cypherlite/cypherlite/src/commit.rs`) holds the publication write guard and the WAL mutex for the whole group, evaluates slots in commit order, and already fails one slot without failing the group: a write-set conflict rejects that slot with `Error::Conflict`, the rest of the group still commits, and nothing is appended for the rejected slot. Committed state and the group's own accepted writes are both visible there.

What the loop lacks is an exact per-constraint value probe. The statement-time validator probes shared lazy equality postings: per `(label, key)` for nodes or `(type, key)` for relationships, a superset of every version of every candidate, built by walking the whole label bucket or every relationship on first probe, then re-verified candidate by candidate. They sit outside the [ADR-0042](./0042-graph-memory-limit.md) charge ([ADR-0172](./0172-incremental-constraint-validation.md) Consequences). The materialized constraint backing index is a per-property range index, node-only. Neither answers "does this full tuple exist for this constraint in committed state" exactly, and neither has any state for a value introduced earlier in the same flush group.

## Decision

`flush_group` gains a commit-time uniqueness admission check, backed by a maintained, single-version per-constraint tuple map on `Graph`.

### The probe

A new module family `graph/constraint_keys/` owns the map and the admission side. `mod.rs` holds the map vocabulary, its byte constants, and the backfill/rebuild lifecycle; `seams.rs` holds the `apply_redo`-seam maintenance; `simulation.rs` holds the one redo-op classification, the constrained `KeyView`, and the slot simulation; `ddl.rs` holds the batch DDL accumulator and the one net-transition projection both the probe and the estimate drive; `admission.rs` holds the commit-loop probe; `estimate.rs` holds the entry-growth estimate. The dependency runs one way: the admission and estimate sides import the map's vocabulary, the map never names them. The field sits on `Graph` beside `label_counts` and `pair_counts`, not in `Indexes`: that bundle is lazily maintained, while this map is eagerly current.

- **Key**: the constraint identity (its catalog name) and the full tuple, a `Box<[GroupKey]>` in the constraint's `properties` order, each component built with `GroupKey::for_property_index`. That is the equivalence key the existing probes use ([ADR-0172](./0172-incremental-constraint-validation.md) §3, [ADR-0184](./0184-composite-key-smallest-bucket-probe.md), [ADR-0185](./0185-relationship-equality-postings.md)).
- **Value**: a `TupleEntry` of `holders: Vec<EntityId>` and `charged: u64` — the live entities holding the tuple and the bytes this entry was charged, so removal refunds exactly what creation charged. `EntityId` is `Node(NodeId)` or `Rel(RelId)`; the constraint identity fixes which variant.
- **One holder per live constrained entity per constraint.** Entries are keyed by the tuple, so the normal case is one entry with one holder; a second holder is a pre-existing violation recorded by replay or a replicated record, never installed by admission.
- **Unkeyable tuples**: a null or NaN component produces no entry. Nulls never equal under a `unique` constraint (several nulls may coexist), a NaN never equals itself, and a `key` constraint's missing property is the required-property check, not uniqueness. An entity with such a tuple can neither violate nor be violated.
- **Composite constraints** get one true composite key. This does not change [ADR-0184](./0184-composite-key-smallest-bucket-probe.md)'s smallest-bucket choice for the statement-time validator; at commit time the whole tuple is the key, so bucket cardinality and declaration order cannot matter.
- **Relationship constraints** use the same map with `RelId` as the holder. They have no backing index postings at all, so this is their first exact committed-state probe. They are in scope even though LABS-551's reproducer is node-only: the same map and the same seams cover them at no extra design cost, and carving them out would preserve the identical gap for `RELATIONSHIP KEY`.

Single-version state is sufficient: the check runs under the publication write guard, serialized against every other commit and against reload, and only current committed state matters. A rejected slot never applies; a retry runs on a fresh snapshot after this group applies.

**Lock order.** The map lock is taken last, after the entity (`nodes` / `rels`) and `label_index` guards. No path holds the map lock while acquiring an entity guard: the admission probe builds its tuples from committed entities first, then takes the map lock for the lookups, matching the `apply_redo` primitives' order. The entry-growth estimate snapshots the map's specs, per-constraint charges, and scope under the lock, releases it for the simulation (which interns DDL names and takes entity guards), and re-takes it only for the per-entity refund queries after the simulation is done.

The probe is O(P) to build the tuple's keys and O(1) map operations (hash, insert, remove) per constrained entity, independent of label or type cardinality. One admission check costs O(ops × P), independent of graph size.

### Op vocabulary

The map changes at the `apply_redo` primitive seams. For each mutating op the one `ConstraintKeyScope` covers (the delete seams delegate to the transition seam with no after-image instead, per Maintenance below), the primitive frees the entity's before tuple and introduces its after tuple, per applicable uniqueness constraint:

- **Introduce**: `CreateNode` (its labels and properties), `CreateRel` (type and properties), `SetNodeProp` / `SetRelProp` (a new constrained value, or filling a missing key property), `AddLabels` (newly placing a node under a node constraint's label).
- **Free**: `DeleteNode`, `DeleteRel`, `RemoveNodeProp`, `RemoveRelProp`, `RemoveLabels`. `SET x = null` is encoded as a `RemoveNodeProp` / `RemoveRelProp` at the write primitive, so it takes the free path with no extra case.
- **Ddl**: `CreateConstraint`, `DropConstraint`, `CatalogDerivedConstraint`, `SetCurrentGraphType` change the maintained constraint set, not a tuple. The admission loop handles each directly — install, drop, or a graph-type re-derivation whose derived constraints install like a catalog-derived one. The entry-growth estimate prices a new uniqueness constraint's backfill from the exact `label_counts` / relationship-type count and refunds a drop, and treats `SetCurrentGraphType` as an explicit no-op: on the statement path the derived constraints it installs arrive as the `CreateConstraint` rows the same statement emits before the `SetCurrentGraphType` push, and those rows are what the estimate prices — except a derived constraint the skip rule suppressed as schema-equivalent (Rebuild shape b), which gets no row and whose rebuild-installed entries the estimate does not price: the estimate is a lower bound for a graph-type ADD over a schema-equivalent catalog constraint. Apply is not the estimate: the `SetCurrentGraphType` apply arm replaces the derived set wholesale from the persisted body and rebuilds the map (the fourth rebuild trigger, see Rebuild), because the row stream is not a complete mirror of that set.
- **Irrelevant**: every other op. No redo op touches a constrained tuple or the constraint set.

The classification is one exhaustive function, `constraint_effect(op) -> ConstraintOp` (`Introduce` / `Free` / `Ddl` / `Irrelevant`, carrying the op's borrowed payload) in `graph/constraint_keys/simulation.rs` — no wildcard arm, so a new `RedoOp` variant cannot compile without a decision. `SlotSimulation::simulate` / `simulate_redo`, the admission loop, `is_constraint_ddl` and the entry-growth estimate are all projections of that one value; none re-derives the classification with its own `match`, and the loops hand the classified `TupleOp` straight to the simulation instead of re-classifying it.

A free can only help a later slot, but it must update the map so a later slot in the same group sees the value as available. A delete can never introduce a violation. Non-constrained properties and the existence/endpoint-label kinds (`ConstraintType::enforces_uniqueness` is false for them) are not in the map. Re-asserting the tuple the entity already holds is a no-op, not a self-collision.

### One scope, owned by the map

The map owns a `ConstraintKeyScope`: the union of every effective constraint's label (or relationship type) and property ids. It is recomputed from the map's own spec set in exactly one place, on every install, drop, and rebuild, and every consumer asks that one value:

- the admission probe classifies each op against it, so an op outside it never materializes an entity view — except a create, which always registers its (scope-filtered) view: the create is the only op that establishes an entity's existence in the simulation, and a later op in the same slot (a label add, a key `SET`) can pull that entity into scope, so skipping the registration would let the later op no-op against a `None` seeded from committed state;
- every mutating `apply_redo` seam (`install_node`, `install_rel`, `set_property_node`, `set_property_rel`, `add_labels`, `remove_labels`) gates its before-image snapshot on it; the delete seams (`constraint_keys_delete_node`, `constraint_keys_delete_rel`) take the entity guard and delegate to the transition seam with a `None` after-image (the transition keeps the map's `is_empty` guard) — the map is the authority there, no scope needed;
- both create seams (`install_node`, `install_rel`) skip the map entirely when the new entity's labels/type and keys are outside it.

Re-deriving this set per consumer is parallel state over one fact. A maintenance gate narrower than the check gate would skip recording a value the probe later assumes is in the map — a false negative, i.e. exactly the duplicate commit this decision closes. The admission engine snapshots the same scope at group start and widens it only with the domain of a constraint installed earlier in the group (that constraint has no committed map yet); it never re-derives the committed part.

### Conflict reply: `Error::Conflict`

A slot that would introduce a tuple already held by another live entity fails with `Error::Conflict`, by the same slot-failure path as an element-id write-set conflict. The probe runs in the per-slot loop beside `write_set_conflicts` and before the capacity reservation, under the already-held publication and WAL guards, in commit order: `waiters.reject(index, Error::conflict())`, the slot's ops are dropped and never appended, and the rest of the group still commits if valid.

Decided over replying with `Error::UniquenessViolation` directly. The trade-off:

- `Error::Conflict` is one vocabulary for every admission conflict. Every surface already classifies it: the C ABI as `("WriteConflict", "SemanticError")`, Bolt as `Neo.TransientError.Transaction.Outdated` (managed transactions retry), the CLI as retryable with exit 5.
- The retry produces the precise error. A new commit attempt blocks on the publication guard until this group's `apply_redo` finishes, so the retry's snapshot contains the winner; its statement-time validator then raises `Error::UniquenessViolation` with the pinned `ConstraintValidationFailed: <name> uniqueness` text.
- A retried `MERGE` can converge by matching the winner instead of failing.
- The cost: a client that does not retry sees `write-write conflict` rather than the semantic violation. That is the accepted cost of optimistic admission and is identical to the element-id conflict contract ([ADR-0017](./0017-client-conflict.md)); a data-error reply at admission would instead make a transient race permanent.

The direct `UniquenessViolation` alternative is rejected in Alternatives considered.

### Same-group ordering

Slots are evaluated in commit order, exactly like the element-id checks. The effective state for a slot is the committed map plus the delta every earlier accepted slot left, applied in slot order; within a slot, its own ops apply in redo order.

- An earlier accepted introduction wins; the later slot fails.
- An earlier accepted free — a delete, a property removal, or a label removal — frees the tuple for a later slot.
- A free earlier in the same slot frees the value for an introduction later in that slot.
- A slot fails at its first introduction that would add a second live holder, whether the first holder is committed, in an earlier accepted slot, or earlier in the same slot.

This is the same ordering `redo_conflicts_with_later` already establishes for element ids: earlier accepted slots stand in for committed state. Two accepted slots never touch the same entity (the element-id check rejects that), so each slot's before-image is the committed graph's, read under the guards; the overlay only tracks tuple occupancy and constraint install/drop markers.

The overlay is built only for the slots that can need it. A group whose map holds no constraints and whose ops carry no constraint DDL skips the simulation entirely, and the last slot of a group whose ops can only free (or touch nothing) returns before the simulation too: no later slot can read that overlay, and the statement-time validator already validated the slot's own writes. The bulk `DETACH DELETE` shape takes both shortcuts.

Constraint DDL in the group is part of that overlay. An earlier accepted `DropConstraint` (or a derived-constraint removal) removes its keys from the effective state, so a later slot may reuse the value. A uniqueness constraint installed earlier in the group has no committed map yet; rather than build a second O(L) backfill inside the admission loop, a later slot that introduces a value into that constraint's domain and a slot installing the constraint over an earlier accepted slot's introductions both fail with `Error::Conflict`. The retry then sees the committed constraint (or data) and the statement-time validator gives the precise verdict. This is a rare DDL race, and the conservative reply is exact by retry without a second scan.

### Maintenance, backfill, rebuild, charge

**Maintenance.** The map changes at the `apply_redo` primitive seams — `install_node`, `install_rel`, `set_property_node`, `set_property_rel`, `delete_node`, `delete_rel`, `add_labels`, `remove_labels`. Every mutating seam asks the map's one `ConstraintKeyScope` first: a mutation whose labels/type and keys are all outside it returns without touching the map and without snapshotting an entity. Only a constrained mutation reads the before-image and computes the entity's before and after tuples for the maintained constraints — the create seams skip the map entirely for an unconstrained new entity, the SET/REMOVE seams snapshot only when the mutated key is constrained, and the label seams only when one of the moved labels is. The delete seams are the stated exception: they do not consult the scope but read the live version under the entity guard and delegate to the transition seam with no after-image (the map, whose `is_empty` guard the transition keeps, is the authority there), so the map lock is taken last and the tuple is freed without cloning the entity. `apply_replayed_redo` shares every seam. Applying an op is a state recorder, never a checker: a duplicate found at apply (possible on legacy replay or a replicated record) appends a second holder instead of failing a durable group. Admission is the only rejecting path.

**Constraint lifecycle.** Applying a `CreateConstraint` backfills from the live graph and charges the entries; `DropConstraint` removes the constraint's entries and refunds. The backfill installs every catalog uniqueness constraint, including a derived one that the graph-type sync emitted as a `CreateConstraint` row (the Ddl bullet above); the `SetCurrentGraphType` wholesale rebuild (Rebuild below) is the backstop for derived names that have no row — the schema-equivalent skip case and a legacy replay carrying `SetCurrentGraphType` alone. The node backfill reads the constraint label's `label_index` bucket (an upper bound that can still carry an id whose last version is GC-pending), liveness-checks each candidate, and records one holder per live entity: O(L) over the live entities of that label, not an O(N) walk of the graph. A relationship backfill walks `rels` because there is no relationship-type index: O(R). The constraint-creation statement already pays these bounds for its recheck ([ADR-0174](./0174-committed-catalog-constraint-rechecks.md)); this is the same read, once.

**Rebuild.** `rebuild_indexes` (open, checkpoint restore, `abort_write`) rebuilds the map wholesale from the live graph, like `label_counts`, and preserves a pre-existing duplicate: a tuple with more than one live holder is rebuilt with every holder instead of collapsing to one. The map is not checkpointed; it is reproducible from the live graph and its charge is re-tallied by `recompute_used`.

The `SetCurrentGraphType` apply arm is the fourth trigger. It replaces the theater's derived set wholesale from the persisted body and then rebuilds the map from the committed catalog plus that re-derived set. The per-op `CreateConstraint` / `DropConstraint` rows the same statement emits are the fast path and cover every derived constraint they name, but they are not a complete mirror of the re-derived set in two reachable shapes: (a) a legacy replay record carrying `SetCurrentGraphType` alone, with no `CreateConstraint` rows — the redo model and `apply_replayed_redo` accept that record shape; (b) a current statement whose `sync_derived_constraints` skipped a derived constraint as schema-equivalent to a catalog constraint that survived the purge, so that derived name has no row either. One test pins each shape; both would leave a derived uniqueness constraint out of the map, and the rebuild restores the invariant that the map covers the committed catalog ∪ the theater's derived set. Dropping it is therefore rejected: it would make the map depend on the statement's row diff mirroring the body derivation exactly, which the skip rule already violates.

**Charge (the LABS-351 discipline).** The module owns a `CONSTRAINT_KEY_ENTRY_BYTES`-style family: fixed entry overhead (`48 + 64` for the map slot and allocator floor, the same floor-estimate style as the degree-stat maps) plus the sum of `GroupKey::logical_bytes()` for the tuple plus one entity id per holder (`HOLDER_BYTES`). Because tuples are variable length, each entry records the bytes it was charged so removal refunds exactly what creation charged. Charge on entry creation, refund on removal. Include the family in `structure_bytes` / `recompute_used` (a sibling of `degree_stats_structure_bytes`, or the recount drifts from the incremental charge — the #398 lesson) and add an entry-growth term to `estimate_redo_delta`, so `admit_redo_capacity` refuses an over-cap write at admission rather than after its entries land. For a constraint install the estimate may derive the entry count from the exact `label_counts` / relationship-type count in O(1); the backfill itself still needs the ids from `label_index` / `rels`.

### Closed constraint-kind vocabulary

- **What was tightened.** `Catalog::insert_constraint` (`cypherlite/cypherlite-catalog/src/lib.rs`) rejects any `ConstraintDef.kind` that `ConstraintType::from_str` cannot parse, with the typed semantic error `ConstraintValidationFailed: <name> unknown constraint type "<kind>"` — the same text `ConstraintCheck::new` already fails closed with — before the duplicate/equivalent checks and before the row enters the catalog. The vocabulary is closed because every executor-authored definition spells its kind with `ConstraintType::as_str()`. This is the boundary `spec_from_def`'s `expect` rests on: a catalog-accepted definition always parses, so the commit-time map never has to guess whether an unparsable tag is a node constraint or a non-uniqueness kind.
- **Consequence for durable load.** `insert_constraint` is a load path, not only a statement path: checkpoint restore installs every manifest constraint through it (`Graph::install_named_constraint`, `snapshot/native/mod.rs`), and WAL replay validates each `CreateConstraint` through the redo preflight's candidate catalog (`RedoPreflight::insert_constraint`, `graph/redo_preflight.rs`) before apply. A legacy store whose checkpoint manifest or WAL carries an unknown constraint-kind tag now fails to open or replay with that typed semantic error instead of loading the row silently. Kinds `ConstraintType` knows but that do not enforce uniqueness (`exists`, `propertyType`, the existence/endpoint-label family) are unchanged: they parse, install, and get no map entry (`spec_from_def` returns `None`).

### Trust boundary and scope

- **Single process.** The commit loop is the only write serialization point. There is no cross-process or cluster coordination.
- **[ADR-0015](./0015-optimistic-wal-commits.md) write-skew is unchanged beyond constrained values.** Read/write dependencies that do not touch a constrained tuple still commit. This decision adds a constraint-value check, not serializability.
- **The WAL/replay path is trusted.** `apply_replayed_redo` records what the log says, including pre-existing duplicates from legacy images or WALs. A second holder marks such a tuple occupied without failing replay; [ADR-0172](./0172-incremental-constraint-validation.md)'s report-on-touch behavior is unchanged, and any new write touching the tuple is still rejected at admission.
- **The replica apply path shares that trust.** `Database::apply_replicated` (`cypherlite/cypherlite/src/lib.rs`) delegates to `commit_replicated` (`cypherlite/cypherlite/src/commit.rs`), which fenced-checks and runs the capacity admission, then applies through the same `commit_preflighted` → `Graph::apply_redo` pipeline as the group leader. It therefore hits the identical `apply_redo` seams and maintains the map by the same free/introduce recording. It never runs the commit-time uniqueness probe — `flush_group`'s `constraint_admission.check_with_earlier` is leader-only — because a replicated record is trusted to have been admitted on its source; a duplicate it carries records a second holder exactly like legacy replay.
- **The statement-time validator stays.** It reports a violation at the statement that caused it and covers a transaction's own uncommitted writes; the committed map must not answer those. The two checks are complementary, not duplicates.

### [ADR-0174](./0174-committed-catalog-constraint-rechecks.md) Later-references note

The note below landed in [ADR-0174](./0174-committed-catalog-constraint-rechecks.md)'s Later references with this change, because [ADR-0174](./0174-committed-catalog-constraint-rechecks.md) is Accepted and immutable.

```text
- [ADR-0439](./0439-commit-time-uniqueness.md): the pre-existing concurrent duplicate-commit gap recorded in
  Consequences is closed at the commit admission loop, not by item 1's
  mid-transaction recheck: a maintained per-constraint committed-state tuple
  map rejects the later writer with `Error::Conflict` before the WAL append,
  and the client's retry then sees the winner and reports the uniqueness
  violation. Item 1's fingerprint recheck and its semantics are unchanged.
```

## Alternatives considered

- **Status quo: per-statement validation only.** No new state and no admission cost, and the normal single-writer path is untouched. Rejected: it is exactly the gap. Two concurrent transactions creating the same value both commit, in the auto-commit form and in the explicit-transaction-with-no-later-statement form, and no statement-time check can ever see the other transaction's pin.
- **Full write-skew detection (serializable isolation, predicate locks or read-set validation).** Would also catch non-constraint anomalies. Rejected: it prices every read and needs abort/retry machinery for a guarantee beyond the constrained-value contract this change needs; [ADR-0015](./0015-optimistic-wal-commits.md) rejected it for the same reason.
- **Reuse the lazy equality postings at commit time.** No new state. Rejected: a probe costs an O(1) hash plus an O(bucket) clone plus O(bucket × P) re-verification, and the first probe pays an O(L) node / O(R) relationship pair build; the postings are a superset, are per-property and split between node and relationship twins, so a composite tuple still needs the smallest-bucket heuristic; they are outside the [ADR-0042](./0042-graph-memory-limit.md) charge; and they hold no state from earlier accepted slots, so a group-local overlay would still be required.
- **Probe the materialized backing range index.** Already materialized and node-only seeks work. Rejected: it is per-property, not per-tuple, so it cannot express a composite key; it materializes nothing for a relationship constraint; and it is a superset requiring re-verification, with no group overlay.
- **Full recheck of the constraint at commit.** Reuses the [ADR-0172](./0172-incremental-constraint-validation.md) §5 recheck. Rejected: O(L) or O(R) per constraint per commit, O(L × C) over an ingest — the quadratic cost [ADR-0172](./0172-incremental-constraint-validation.md) removed.
- **A committed, versioned unique-value index per constraint** ([ADR-0172](./0172-incremental-constraint-validation.md)'s rejected alternative). Exact O(1) probes including old snapshots. Rejected again: it needs commit-ts MVCC versioning, GC, checkpoint restore, and accounting. A commit-time check runs serialized under the publication/WAL guards and needs only current committed state, so the versioning is unused cost.
- **Reply with `Error::UniquenessViolation` directly at admission.** Skips the retry and shows the precise error immediately. Rejected: it makes an admission-time serialization outcome non-retryable, so a concurrent `MERGE` that could match the winner fails instead; every surface would have to reinterpret an admission reject as a data error; and a genuinely transient race would surface as a permanent violation.
- **A second entity-to-tuple reverse map.** Would make frees O(1) without recomputing the before tuple. Rejected: the primitives already have the before-image and the admission guards pin committed state, so the tuple is computable in O(P); a second table is parallel state that can drift (the code-shape rule).
- **In-engine retry, or queueing the writer until the value clears.** Rejected: [ADR-0017](./0017-client-conflict.md) rejects in-engine retry; queueing is not optimistic ([ADR-0015](./0015-optimistic-wal-commits.md)) and stalls the connection.

## Consequences

- Two concurrent transactions that create or set the same constrained value: exactly one commits. The other gets `Error::Conflict` at admission, its ops are discarded, and nothing is appended. This covers both forms that commit duplicates today: auto-commit and the explicit transaction with no later statement.
- On retry the client sees `Error::UniquenessViolation` with the pinned `ConstraintValidationFailed` text; a retried `MERGE` may instead match the winner and succeed.
- Composite and relationship `unique` / `key` constraints are enforced at commit time with one exact tuple key. [ADR-0184](./0184-composite-key-smallest-bucket-probe.md)'s statement-time probe is unchanged.
- Memory grows by one charged holder per live constrained entity per constraint (one entry per distinct tuple); the charge is in the recount oracle and the admission estimate, and is refunded on removal or constraint drop.
- `CREATE CONSTRAINT` over an existing graph backfills O(L) over the label's live entities (from `label_index`, liveness-checked) or O(R) for a relationship constraint (from `rels`); open, restore, and abort rebuild wholesale in `rebuild_indexes` at the same bounds. A graph-type switch is the fourth rebuild trigger: the `SetCurrentGraphType` apply arm rebuilds wholesale after the per-op `CreateConstraint` rows installed the derived constraints it could name, so a legacy record without those rows and a derived constraint skipped as schema-equivalent still land in the map.
- The WAL/replay path stays trusted: legacy duplicates are recorded, not rejected, and [ADR-0172](./0172-incremental-constraint-validation.md)'s report-on-touch behavior for pre-existing violations is unchanged.
- A uniqueness-constraint install and a write into its domain in the same flush group reject the later slot with `Error::Conflict`; the retry resolves against the committed state.
- The contract documents move with the code: `docs/limitations.md`'s duplicate-commit sentence, [ADR-0174](./0174-committed-catalog-constraint-rechecks.md)'s Later references note (applied above), and the Rust spec's `Conflict` error entry, which repeated the claim.
- The stale comment at `cypherlite/cypherlite-executor/src/graph/index.rs:271-279` is fixed: it said a constraint's backing index "materializes no postings", but LABS-328 made it materialize seek postings. No later text may repeat the stale claim.
- The catalog's constraint-kind vocabulary is closed (`Catalog::insert_constraint`): an unknown kind tag in a durable catalog or WAL now fails open/replay with the typed `ConstraintValidationFailed` semantic error instead of loading silently (see Closed constraint-kind vocabulary).
- No behavior change for statements that touch no constrained value. The admission probe classifies each op against the effective constraints' label/type/property ids first: an op whose label, relationship type, or SET/REMOVE property is not constrained is skipped without materializing any entity, except a create, which always registers its scope-filtered view (see One scope). A touched constrained entity is represented by its constrained properties only (interned ids in the store's existing `CompactProps` shape, `O(P)` values; never the whole node/relationship, and never a second per-view props representation), one view per entity per slot, and the apply seams recompute a tuple only when the mutated property or label can change it. The cost is therefore `O(ops)` classification plus `O(P)` tuple work and `O(1)` map operations per constrained entity, with no full-entity materialization on the constrained path.
- A derived constraint's `entity_type` that names neither `NODE` nor `RELATIONSHIP` is corruption, not a node constraint: every producer renders `EntityKind::display_name()`, and checkpoint restore rejects any other spelling. Two seams classify the record *before* the redo preflight runs — the entry-growth estimate on the replicated commit path (`estimate_redo_delta`) and the group-commit admission probe (`check_with_earlier`, driven by `flush_group`, which never sees a replicated record) — so `entity_kind`/`spec_from_model` are total on both: an unclassifiable value is unpriceable and installs no key — never a panic, and never a silent node classification — while the preflight (`RedoPreflight::check_op`) rejects it with the typed storage error before apply. The `expect` in `spec_from_def` is a post-preflight invariant of the closed constraint-kind vocabulary only.
- Tests for the code change: concurrent same-value auto-commit and explicit-transaction forms; same-group later-slot loss and earlier-free wins; every introduction path (CREATE, SET, AddLabels); composite node-key and relationship constraints; no false rejections across the existing suites; and the memory recount oracle matching the incremental charge after backfill, rebuild, and abort.
