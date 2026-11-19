---
type: adr
area: engine
date: 2026-10-06
deciders: Oskar Hane (agreed in conversation), CypherLite maintainers
name: 0172-incremental-constraint-validation
status: accepted
tags:
  - catalog
title: Incremental constraint validation
REFERENCES:
  - "[[adr/0015-optimistic-wal-commits]]"
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0063-lazy-string-index-postings]]"
  - "[[adr/0075-label-only-snapshot-census]]"
  - "[[adr/0155-bulk-writes-batched-unwind]]"
  - "[[adr/0158-streaming-single-process-pull]]"
  - "[[adr/0171-lazy-per-label-snapshot-probe]]"
  - "[[adr/0173-overlay-owned-redo-log-and-scratch-index]]"
  - "[[adr/0174-committed-catalog-constraint-rechecks]]"
  - "[[adr/0184-composite-key-smallest-bucket-probe]]"
  - "[[adr/0185-relationship-equality-postings]]"
  - "[[adr/0439-commit-time-uniqueness]]"
---

# 0172. Incremental constraint validation

## Context

Constraint validation runs after every write-capable statement
(`execute`, `cypherlite/cypherlite-executor/src/exec/mod.rs:101`). It also
runs after every batch of `CALL { … } IN TRANSACTIONS`
(`cypherlite/cypherlite-executor/src/exec/call.rs:534`). Both call sites call
`validate_constraints` (`cypherlite/cypherlite-executor/src/exec/ddl.rs:274-379`).
An autocommit statement runs on a fresh `WriteScratch`
(`cypherlite/cypherlite/src/commit.rs:96-113`). An explicit transaction runs
every statement on the one scratch it created at `BEGIN`
(`cypherlite/cypherlite/src/connection.rs:217-227`, `:724`).

`validate_constraints` scans the whole graph:

1. **It scans every node or relationship id.** It returns early only when
   there are no constraints (`ddl.rs:275-278`). Otherwise it goes through every
   named constraint (`ddl.rs:280`):
   - A relationship constraint loops over `graph.all_rel_ids()` (`ddl.rs:308`)
     and reads each relationship in full with `graph.rel(id)` (`ddl.rs:309`).
     Only then does it filter by type (`ddl.rs:312`).
   - A node constraint loops over `graph.all_node_ids()` (`ddl.rs:342`), reads
     each node with `graph.node(id)` (`ddl.rs:343`), then filters by label
     (`ddl.rs:346`).

   So a one-row write reads every node or relationship in the graph once per
   constraint: O(C·N) full reads, each cloning a property map.
2. **Uniqueness uses a linear `Vec` scan.** Values of the constrained label
   are pushed into `seen: Vec<Vec<Value>>` (`ddl.rs:306`, `:419`). Each new
   entity is compared against all of them with `seen.iter().any(…
   eq_non_null …)` (`ddl.rs:407-413`). One statement therefore costs
   O(L²) comparisons, where L is the size of the constrained label: about
   5·10⁷ comparisons per statement at 10,000 nodes.
3. **`ExecGraph::all_node_ids` is quadratic in the scratch.**
   `ExecGraph::all_node_ids` (`cypherlite/cypherlite-executor/src/overlay/mod.rs:760-773`)
   collects the snapshot ids and keeps those that still exist. Then, for every
   scratch node, it calls `ids.contains(&id)` (`overlay/mod.rs:766`), a linear
   search of the `Vec`. The cost is O(N·S), where S is the number of nodes the
   scratch holds (created or copied on write). `all_rel_ids` does the same
   (`overlay/mod.rs:781`).
4. **Schema resolution grows with the scratch.** `show_schema()` runs once per
   validation (`ddl.rs:279`, `overlay/mod.rs:307-324`). It clones the committed
   theater (`graph/mod.rs:1105-1107`, `graph/theater.rs:224-229`). Then it
   replays the scratch's entire `pending_redo` through `apply_overlay`
   (`graph/theater.rs:236-286`). `named_constraints()` replays the log again
   (`overlay/mod.rs:412-451`). Both pass over every redo op even though only
   catalog ops matter. In an explicit transaction that log holds the writes of
   every earlier statement.

**Constraint kinds.** `ConstraintType::from_str` accepts ten tags
(`cypherlite/cypherlite-model/src/schema.rs:123-142`): `unique`, `key`,
`exists`, `propertyType`, `labelExistence`, `nodeLabelExistence`,
`sourceLabel`, `sourceLabelExistence`, `targetLabel`, `targetLabelExistence`.

- `enforces_uniqueness` is true for `unique` and `key` (`schema.rs:88-90`).
- `enforces_property_presence` is true for `exists` and `key`
  (`schema.rs:97-99`).
- `propertyType` stores a canonical type that is matched with
  `eval::value_matches_type` (`ddl.rs:294-305`, `:395-405`).

`validate_constraints` also matches six tags by string, all from the same
ten:

- `"sourceLabel" | "sourceLabelExistence"` (`ddl.rs:317`)
- `"targetLabel" | "targetLabelExistence"` (`ddl.rs:318`)
- `"labelExistence" | "nodeLabelExistence"` (`ddl.rs:349-352`)

These check the enforced label E from
`show_schema().data.derived_constraints[name].enforced_label`
(`ddl.rs:281-285`). That entry is absent for legacy-unenforced definitions
(`graph/theater.rs:341-359`), so those skip the check.

The tags come from two places:

- The parser:
  - `nodeLabelExistence`, `sourceLabelExistence` and `targetLabelExistence`
    (`cypherlite/cypherlite-parser/src/parser/schema.rs:522-545`), plus a
    `CatalogDerivedConstraint` op
    (`cypherlite/cypherlite-executor/src/exec/catalog_schema.rs:341-355`).
  - `unique`, `exists`, `key` and `propertyType` (parser `schema.rs:602`,
    `:612-652`).
- Graph-type derivation (`cypherlite/cypherlite-executor/src/graph_type/derive.rs`):
  `sourceLabel`, `targetLabel`, `labelExistence`, `propertyType`, `key`,
  `unique` and `exists`. These are created by `ALTER CURRENT GRAPH TYPE`
  (`catalog_schema.rs:485-517`), which also pushes `SetCurrentGraphType`.

`CREATE CONSTRAINT` rejects any other tag (`catalog_schema.rs:259`).

**Backing indexes.** For a `unique` or `key` constraint,
`constraint_kind_is_index_backed` (`cypherlite/cypherlite-catalog/src/lib.rs:36-38`)
only creates a catalog `IndexDef` named after the constraint (`ddl.rs:26-47`).
That index:

- arrives as `RedoOp::CreateIndex`;
- is installed into the catalog only (`graph/apply_redo.rs:77-87`);
- materializes no postings. Only `CatalogIndexDetail` builds range postings
  (`graph/apply_redo.rs:150-165` → `graph/declared/mod.rs:61-70`).

`Graph::lookup_nodes_at` uses equality postings only for range-indexed
pairs. Every other pair falls back to a label scan (`graph/index.rs:209-211`).
Relationships have no property or type postings at all: `covered_pairs`
returns nothing for a relationship index (`redo/mod.rs:149-152`).

`Graph::prop_postings` (`graph/index.rs:68-87`) builds equality postings
lazily for any `(label, key)` pair. They follow the [ADR-0063](./0063-lazy-string-index-postings.md) superset
contract and are keyed by `GroupKey::for_property_index`. Commit-time dirty
marks keep them current (`graph/index.rs:25-37`, `graph/mod.rs:1263-1269`).

`WriteScratch.prop_ix` (`overlay/mod.rs:30`, `:116-149`) is the scratch's
lazy `(label, key) → GroupKey → ids` overlay. Today only `create_node` adds to
it after it is built (`prop_ix_add`, `overlay/mod.rs:996`).

**Equality today.** A tuple takes part only if all its values are non-null
(`ddl.rs:406`). Tuples are then compared with `Value::eq_non_null`
(`cypherlite/cypherlite-core/src/value.rs:209-256`). Int/Float equality is
exact (`canonical_key::eq_i64_f64`), so `1 = 1.0` but
`9007199254740993 ≠ 9007199254740992.0`. This is the rule pinned by
`cypherlite/cypherlite/tests/property_index_numeric_equivalence.rs`. NaN
equals nothing, and lists compare elementwise.

A throwaway probe on 2026-10-06 confirmed the current behaviour:

- `CREATE (:P {id: 1})` then `CREATE (:P {id: 1.0})` fails with
  ``ConstraintValidationFailed: `p_id` uniqueness``. So does `[1]` then
  `[1.0]`.
- Two NaN ids are accepted.
- Two composite tuples that are both missing a property are accepted.
- A duplicate across two statements of one explicit transaction is
  rejected.

**Concurrency.** Validation sees only the statement's snapshot plus its
scratch. Commit conflicts look at ids only (`graph/conflict.rs:89-164`), and
[ADR-0015](./0015-optimistic-wal-commits.md) rejected write-skew detection. The probe showed the effect: two
explicit transactions that each create `(:P {id: 77})` both commit. After
that, **every** later write statement fails with `p_id` uniqueness,
`CREATE (:Other)` included, because the full scan finds the committed
duplicate again.

**Why this matters now.** Batched ingest ([ADR-0155](./0155-bulk-writes-batched-unwind.md), [ADR-0158](./0158-streaming-single-process-pull.md)) issues many
write statements against a growing graph. At O(C·N + L²) per statement, a
load is quadratic or worse. [ADR-0171](./0171-lazy-per-label-snapshot-probe.md) removes the other per-statement O(N)
term, the label census. This ADR removes the constraint term.

## Decision

Constraint validation becomes **incremental**:

- A validation re-checks only what the writes since the previous validation
  could have broken.
- It also fully re-checks any constraint created or re-derived in that span.
- Its accept/reject decisions and error texts are identical to today's
  whenever the state before those writes satisfied every constraint.
  Each successful validation establishes that state.
- The entry point `validate_constraints(graph)` and both call sites
  (`exec/mod.rs:101`, `exec/call.rs:534`) stay unchanged.

### 1. The validation slice: how touched entities are tracked

Every `ExecGraph` write primitive appends a redo op to
`WriteScratch.pending_redo` for each call that writes. A call that changes
nothing pushes nothing: removing a missing property, adding a label the node
already has, removing a label it lacks. The primitives are `create_node`,
`create_rel`, `add_labels`, `remove_labels`, `set_property_node`,
`set_property_rel`, `delete_rel` and `delete_node` (`overlay/mod.rs:985`,
`:1016`, `:1090`, `:1113`, `:1146`, `:1178`, `:1233`, `:1267`). Map `SET`,
`+=`, MERGE and dynamic labels all go through them (`exec/write.rs:163-209`,
`:250-297`, `exec/create.rs:293-348`). The redo log is therefore the
scratch's change log.

**Cursor.** `WriteScratch` gains `validated_redo: usize`:

- On entry, `validate_constraints` takes the slice
  `pending_redo[validated_redo..]` and sets `validated_redo =
  pending_redo.len()` before checking anything. This is drain-on-entry.
- `take_redo` resets the cursor to 0.

**Classification.** The slice is classified into hash sets:

| Op in the slice | Recorded as |
| --- | --- |
| `CreateNode`, `AddLabels`, `RemoveLabels`, `SetNodeProp`, `RemoveNodeProp` | **touched node** (its id) |
| `RemoveLabels` (in addition to the above) | **label-loss node** |
| `CreateRel`, `SetRelProp`, `RemoveRelProp` | **touched relationship** |
| `CreateConstraint { name }`, `CatalogDerivedConstraint { constraint }` | name added to the **recheck set** |
| `SetCurrentGraphType` | every constraint with an enforced-label kind (`ConstraintType::is_enforced_label_kind`) added to the recheck set |
| `DeleteNode`, `DeleteRel`, index ops, other catalog ops, `PullIdentityPut` | nothing; deletion can never introduce a violation |

**Liveness and reads.**

- "Carrying L" and "of type T" are evaluated on the live `ExecGraph` view at
  validation time.
- An id that is no longer live is skipped, just as `graph.node(id)` returning
  `None` is skipped today.
- Entities are read with facet accessors, never with `node()` or `rel()`
 ([ADR-0075](./0075-label-only-snapshot-census.md)'s rule): `node_labels`, `node_prop`, `rel_type`, `rel_prop`,
  `rel_ends`.

**Lifetime of the slice.**

- **Autocommit:** the slice is the statement.
- **Explicit transaction:** the slice is the current statement. The cursor
  advanced past the earlier statements when they validated. A statement that
  fails marks the transaction failed (`connection.rs:745`), so a partly
  validated log can never commit.
- **`CALL { } IN TRANSACTIONS`:** the slice is the batch plus any outer writes
  not yet validated. `scratch_snapshot` and `restore_scratch` (`call.rs:515`,
  `:541`) clone and restore the cursor together with the log. A failed batch
  therefore hands its unvalidated outer writes to the next validation.

### 2. What each kind re-validates

In the table, P is the constraint's property list and
`p` ranges over P. L is the label of a node constraint and T the type of a
relationship constraint. E is the enforced label.

Trigger operations:

- **`SET n:L`** means static, dynamic `$(…)` and `labels()` extend or
  overwrite.
- **`REMOVE n:E`** means static, dynamic and `labels()` remove or overwrite.
- **`ON CREATE SET / ON MATCH SET`** means the `SET` forms inside MERGE.
- **`recheck: …`** marks the DDL that puts the constraint in the recheck set
  (section 5).
- `SET e.p = null` and `REMOVE e.p` violate `propertyType` only when its type
  is `NOT NULL`.
- No row triggers on `DELETE` or `DETACH DELETE`.

The rules use the six string-matched tags exactly as today:

- the node label check for `labelExistence` and `nodeLabelExistence`;
- the start-node check for `sourceLabel` and `sourceLabelExistence`;
- the end-node check for `targetLabel` and `targetLabelExistence`.

The node or relationship form follows `ConstraintDef.relationship`.

| Kind tag | Re-validated entities | Trigger write operations |
| --- | --- | --- |
| `unique` | Node form: touched nodes carrying L, each probed for conflicting partners among all live nodes carrying L (section 3). Relationship form: touched relationships of type T, partners found by the fallback scan (section 4). Every live entity of L/T on recheck (section 5). | CREATE node; CREATE relationship; MERGE create branch; SET e.p = v; SET e = map; SET e += map; SET n:L; ON CREATE SET / ON MATCH SET; recheck: CREATE CONSTRAINT; recheck: ALTER CURRENT GRAPH TYPE |
| `key` | As unique (node form: touched nodes carrying L, probed per section 3; relationship form: touched relationships of type T, fallback scan per section 4), each also checked for the presence of every property in P. Every live entity of L/T on recheck (section 5). | CREATE node; CREATE relationship; MERGE create branch; SET e.p = v; SET e.p = null; REMOVE e.p; SET e = map; SET e += map; SET n:L; ON CREATE SET / ON MATCH SET; recheck: CREATE CONSTRAINT; recheck: ALTER CURRENT GRAPH TYPE |
| `exists` | Touched nodes carrying L (node form) or touched relationships of type T (relationship form), each checked for the presence of every property in P; no probe. Every live entity of L/T on recheck (section 5). | CREATE node; CREATE relationship; MERGE create branch; SET e.p = null; REMOVE e.p; SET e = map; SET e += map; SET n:L; ON CREATE SET / ON MATCH SET; recheck: CREATE CONSTRAINT; recheck: ALTER CURRENT GRAPH TYPE |
| `propertyType` | Touched nodes carrying L (node form) or touched relationships of type T (relationship form), each checked with eval::value_matches_type on the first property of P; no probe. Every live entity of L/T on recheck (section 5). | CREATE node; CREATE relationship; MERGE create branch; SET e.p = v; SET e.p = null; REMOVE e.p; SET e = map; SET e += map; SET n:L; ON CREATE SET / ON MATCH SET; recheck: CREATE CONSTRAINT; recheck: ALTER CURRENT GRAPH TYPE |
| `labelExistence` | Touched nodes carrying L, each checked for the enforced label E. Every live node carrying L on recheck (section 5), including when SetCurrentGraphType re-derives E. | CREATE node; MERGE create branch; SET n:L; REMOVE n:E; recheck: ALTER CURRENT GRAPH TYPE |
| `nodeLabelExistence` | Touched nodes carrying L, each checked for the enforced label E. Every live node carrying L on recheck (section 5), including when a CatalogDerivedConstraint for it is pushed. | CREATE node; MERGE create branch; SET n:L; REMOVE n:E; recheck: CREATE CONSTRAINT |
| `sourceLabel` | Touched relationships of type T plus the live outgoing relationships of type T of every label-loss node; each start node checked for E. Every live relationship of type T on recheck (section 5), including when SetCurrentGraphType re-derives E. | CREATE relationship; MERGE create branch; REMOVE n:E (start node); recheck: ALTER CURRENT GRAPH TYPE |
| `sourceLabelExistence` | Touched relationships of type T plus the live outgoing relationships of type T of every label-loss node; each start node checked for E. Every live relationship of type T on recheck (section 5), including when a CatalogDerivedConstraint for it is pushed. | CREATE relationship; MERGE create branch; REMOVE n:E (start node); recheck: CREATE CONSTRAINT |
| `targetLabel` | Touched relationships of type T plus the live incoming relationships of type T of every label-loss node; each end node checked for E. Every live relationship of type T on recheck (section 5), including when SetCurrentGraphType re-derives E. | CREATE relationship; MERGE create branch; REMOVE n:E (end node); recheck: ALTER CURRENT GRAPH TYPE |
| `targetLabelExistence` | Touched relationships of type T plus the live incoming relationships of type T of every label-loss node; each end node checked for E. Every live relationship of type T on recheck (section 5), including when a CatalogDerivedConstraint for it is pushed. | CREATE relationship; MERGE create branch; REMOVE n:E (end node); recheck: CREATE CONSTRAINT |

**Per-entity checks.** These are today's checks in today's order:

1. the label or endpoint check (`ddl.rs:315-326`, `:349-361`);
2. required (`ddl.rs:389-394`);
3. property type (`ddl.rs:395-405`).

A missing property reads as `Null`. A label-loss node's incident
relationships come from `for_each_neighbor(id, Outgoing | Incoming, &[T])`,
which includes scratch adjacency.

**Unknown tags.** A stored tag that `from_str` rejects keeps today's
fail-closed meaning: only the property-type check runs, on touched entities
of L/T, and it is never required and never unique.

### 3. Uniqueness probe for node constraints (no scan)

Take a node `unique` or `key` constraint C = (L, P = [p0, …, pk]). Each
re-validated node x that passed its per-entity checks and whose values V are
all non-null is probed as follows:

1. **Key.** Compute `g = GroupKey::for_property_index(&V[0])`
   (`group_key.rs:276-281`). If it is `None`, V[0] contains a NaN.
   `eq_non_null` is then false against every value, so x has no partner.
   Skip it.
2. **Candidates** are the union of two structures:
   - **Committed side.** Equality postings for `(L, p0)` at `g`, from a new
     `pub(crate) Graph::equality_postings(label, key, value) -> Vec<NodeId>`.
     It wraps `prop_postings` (`graph/index.rs:68-87`), which builds the pair
 lazily ([ADR-0063](./0063-lazy-string-index-postings.md)) and drains its dirty ids. It returns an empty `Vec`
     when L or p0 was never interned. It deliberately bypasses the
     `range_serves_equality` gate of `lookup_nodes_at`
     (`graph/index.rs:209-211`), because the constraint's backing `IndexDef`
     materializes no postings. It uses the same entry point that equality
     seeks use today, so it adds no new lock ordering.
   - **Scratch side.** `WriteScratch::prop_ix_seek(&[L], p0, &V[0])`
     (`overlay/mod.rs:116-149`). Two new updates keep `prop_ix` complete
     across every statement of the scratch:
     - `set_property_node(id, key, v)` with a non-null v inserts id into the
       already-built pairs `(l, key)` for each l in the node's labels plus
       `""`.
     - `add_labels(id, added)` inserts id into the already-built pairs
       `(l, k)` for each added l and each property k of the node.

     Today only `create_node` adds to it (`overlay/mod.rs:996`). The two
     updates follow that existing `prop_ix_add` pattern. They maintain an
     index; the touched set itself needs no hooks (section 1).
3. **Verification.** Each candidate y ≠ x is checked on the snapshot+scratch
   view:
   - y is live;
   - `node_labels(y)` contains L;
   - for every i, `node_prop(y, P[i])` is `Some(w)` with
     `w.eq_non_null(&V[i])`.

   This is exactly today's predicate (`ddl.rs:406-413`). Every verified y is a
   conflict partner of x (section 6).

**Equality** is `Value::eq_non_null` and nothing else. `GroupKey` only picks
the bucket:

- **No conflict is missed.** For NaN-free values, `eq_non_null(a, b)` implies
  equal keys. Both use the same exact Int/Float rule
  (`canonical_key::f64_exact_i64`), same-variant temporal comparison, and
  elementwise lists, maps and vectors.
- **No false conflict is reported.** Pairs whose keys collide but that are
  not `eq_non_null`-equal are removed by verification, for example lists with
  null elements (`Norm::Null` is reflexive, `group_key.rs:180`).

The decisions are therefore today's:

- `1` / `1.0` conflict, and so do `[1]` / `[1.0]`.
- `9007199254740993` / `9007199254740992.0` do not conflict
  (`property_index_numeric_equivalence.rs:100-133`).
- A NaN never conflicts.
- A tuple with any null property is never probed and never conflicts.

### 4. Fallback for relationship `unique` / `key`, which have no backing index

No structure indexes relationship property values. The backing `IndexDef`
treats T as a node label, `covered_pairs` skips relationship indexes, and
`Graph` keeps no relationship-by-type or relationship-property postings. For
each relationship constraint C = (T, P) in a validation:

1. **Bucket the touched values.** Build `touched_keys: HashMap<Vec<GroupKey>,
   Vec<RelId>>` from the re-validated relationships of type T that passed
   their per-entity checks. Include only those whose values are all non-null
   and whose keys `for_property_index` all exist.
2. If `touched_keys` is empty, stop. There is no scan.
3. **Scan once.** Make one pass over `ExecGraph::all_rel_ids()` (fixed in
   section 8). For each id with `rel_type(id) == T` (a stub read, no property
   map), read P with `rel_prop`. Build the key tuple and look it up in
   `touched_keys`. For every touched x in the bucket with x ≠ id, verify
   `eq_non_null` on every component; a match is a conflict partner. Touched
   relationships are in the pass too, so conflicts among them are found.

**Cost:**

- an O(R) id walk plus O(R_T·|P|) property reads, once per such constraint
  per validation;
- zero when the slice touched no relationship of type T with all-non-null
  constrained values;
- never quadratic within a statement.

### 5. Full re-validation for the recheck set

Each constraint in the recheck set that is still present in
`named_constraints()` is checked over **all** live entities of L/T. The check
is today's algorithm in ascending id order, with one change: uniqueness uses
a `HashMap<Vec<GroupKey>, Vec<Vec<Value>>>` bucket instead of the `Vec` scan.

- An entity conflicts if and only if an earlier tuple in its bucket is
  `eq_non_null` on every component.
- A NaN-containing tuple is never bucketed, because it can equal nothing.
- The first failure in id order gives today's error.

This is what `CREATE CONSTRAINT` over existing data costs today, minus the
quadratic term.

### 6. Error selection: identical to today

Today the first failure wins:

- Constraints are scanned in name order (the `BTreeMap` order of
  `named_constraints()`).
- Within a constraint, entities are scanned in ascending id order.
- At one entity, the label or endpoint check runs first, then required, then
  property type, then uniqueness.
- A uniqueness failure is raised at the later id of a conflicting pair.

The incremental validator reproduces that choice. For each constraint, in
name order:

1. Parse `property_type` up front exactly as today (`ddl.rs:294-305`), even
   when no entity is re-validated for it.
2. Run section 5 if the constraint is in the recheck set.
3. Otherwise visit its re-validated entities in ascending id order. Collect
   candidate failures keyed `(position, rank)`:
   - a per-entity failure of x is `(x, 0)`, with the message of the first
     check that failed;
   - a uniqueness conflict of x is `(x, 1)` if some partner has an id
     smaller than x. Otherwise it is `(smallest partner id, 1)`.
4. The constraint fails with its smallest `(position, rank)`. Stop visiting
   once x exceeds the best position found.

The first constraint in name order that fails produces the error.

Under the invariant, an entity that is not re-validated has no per-entity
failure, and every conflicting pair contains at least one re-validated
entity. This choice is therefore the failure today's full scan would hit
first.

The messages, their `Error` variants and the downstream matchers stay
byte-identical:

- ``ConstraintValidationFailed: `{name}` uniqueness``, `requires properties`
  and `property type` are `Error::semantic`.
- `ConstraintValidationFailed: node with label {L} must also have label {E}`
  and `ConstraintValidationFailed: endpoint label presence verification
  failed: expected {E}` are `Error::execution`.
- The matchers are `transaction_error_message` (`call.rs:574-585`) and
  `cypherlite/cypherlite/src/binding.rs:196`.

### 7. Explicit transactions and `CALL { } IN TRANSACTIONS`

- **Explicit transactions.** Each statement validates its slice against the
  transaction's view: the snapshot pinned at `BEGIN` plus the transaction's
  scratch. The scratch-side probe (`prop_ix`) covers every node any earlier
  statement of the transaction created or changed. A duplicate created by an
  earlier statement of the same transaction is therefore still rejected.
- **`CALL { } IN TRANSACTIONS`.** It takes the same path. `call.rs:534` calls
  the same `validate_constraints`, which validates the batch's slice. On
  failure, `restore_scratch` (`call.rs:541`) rewinds the cursor with the log.
  The final validation in `execute` (`exec/mod.rs:101`) covers only the
  writes made after the last batch. Its slice is empty, and it costs O(1),
  when there are none.

### 8. Fixed cost per validation

- **Empty slice.** It returns `Ok(())` before `named_constraints()` or
  `show_schema()` are called.
- **Otherwise,** `named_constraints()` and `show_schema()` run once each, as
  today, but their scratch overlay replays only catalog ops:
  - `WriteScratch` gains `catalog_redo: Vec<usize>`. `push_redo` records the
    position of every op it appends that is not one of the ten entity ops or
    `PullIdentityPut`, and `take_redo` clears it. The ten entity ops are
    `CreateNode`, `CreateRel`, `DeleteNode`, `DeleteRel`, `SetNodeProp`,
    `RemoveNodeProp`, `SetRelProp`, `RemoveRelProp`, `AddLabels` and
    `RemoveLabels`.
  - `named_indexes()`, `named_constraints()` and `show_schema()`'s
    `apply_overlay` (`overlay/mod.rs:365-451`, `:307-324`) iterate those
    positions in order.

  They see the same ops in the same order, so the results are identical. The
  cost is O(catalog ops) instead of O(all redo). `show_schema()` still clones
  the committed theater, which is O(schema size) and independent of data
  size.
- **`ExecGraph::all_node_ids` and `all_rel_ids`.** Scratch ids are tested with
  `binary_search` against the sorted snapshot ids collected first, then
  appended, and the result is sorted once. That is O((N+S) log N) instead of
  O(N·S), with the same output for every caller.

## Alternatives considered

- **Keep the full scan but replace the `Vec` with a hash set.** This removes
  the O(L²) term. But every write statement, and every `IN TRANSACTIONS`
  batch, would still read every node or relationship once per constraint:
  O(C·N) full-record clones per validation. Element-at-a-time and batched
  loads would stay O(N²) overall. Its hashing survives as the section 5
  recheck. Rejected: it fixes the constant, not the scaling.
- **Validate once at commit instead of per statement.** Rejected for four
  reasons:
  - A violation would surface at `COMMIT` instead of at the statement that
    caused it, which is where Neo4j and the TCK report it.
  - Later statements in the transaction would read and build on a state that
    violates constraints, for example a MERGE matching one of two duplicates.
  - `IN TRANSACTIONS` needs a decision per batch for `ON ERROR CONTINUE` and
    `BREAK` (`call.rs:537-568`).
  - Autocommit statements are commits already, so commit-time validation
    needs the same incremental algorithm. It only moves the cost.
- **Hooks in every write primitive that fill a touched set.** Rejected: it
  rebuilds, across the write primitives, the change log the redo already is.
  Every primitive would have to keep its hook in sync, and a future primitive
  that forgot it would silently skip validation. A primitive cannot skip redo
  without losing durability, so the redo cursor cannot drift.
- **A committed, versioned unique-value index per constraint, for nodes and
  relationships.** It gives exact O(1) probes, including for relationships,
  and could later support commit-time write-skew checks. A later ADR can
  still add relationship equality postings if a workload needs them.
  Rejected for now: it needs MVCC versioning by commit ts for transaction
 snapshots, maintenance in apply, GC and checkpoint restore, and [ADR-0042](./0042-graph-memory-limit.md)
  accounting. That is new global state, while lazy equality postings for
  nodes already exist and relationship uniqueness constraints are rare.
- **Probe through `lookup_nodes_at` / `ExecGraph::lookup_nodes`.** Rejected:
  it serves only range-indexed pairs. For a pair backed only by a constraint
  it falls back to `scan_match` (`graph/index.rs:210-211`), which is O(L) per
  probe and O(T·L) per statement.

## Consequences

**Cost.** A validation costs O(catalog ops + schema size) plus, per
re-validated entity, O(applicable constraints × (1 + candidates)). On top of
that:

- O(R + R_T·|P|) per relationship `unique` or `key` constraint whose type was
  touched;
- O(N) or O(R) per constraint in the recheck set.

A statement with no writes pays O(1). Today any write-capable statement pays
O(C·N + L²) whenever constraints exist.

**Lazy postings.** The first probe of a `(L, p0)` pair after the database
opens builds its equality postings from the label index, O(|L|) once. The
existing dirty marks then keep them current. They sit outside [ADR-0042](./0042-graph-memory-limit.md)'s
logical charge (`IndexCharge::from_details`, `memory.rs:31-50`), as the
constraint's backing `IndexDef` already does. Charging them needs a later
ADR.

**Composite constraints.** The probe keys on the first property, which is
also the backing-index property (`ddl.rs:41`). Its candidates are every
entity sharing that first value. A composite constraint whose first property
has low cardinality therefore degrades toward one label scan per touched
entity.

**Unchanged decisions.** Accept/reject decisions and error texts are
unchanged whenever the state before the slice satisfied every constraint and
every stored property type parses. `CREATE CONSTRAINT` guarantees the parse
(`catalog_schema.rs:260-266`).

**Deliberate change: pre-existing violations.** Violations the slice did not
introduce are reported only when a statement touches an involved entity.
Such violations come from:

- concurrent write skew. [ADR-0015](./0015-optimistic-wal-commits.md) does no write-skew detection, and the
  probe in Context reproduces it;
- WAL or checkpoint images written by versions that did not enforce a kind;
- legacy-unenforced constraints.

Today such a violation makes **every** later write statement fail until
someone repairs it by hand. Afterwards, unrelated writes succeed, while
writes touching an involved entity, and `CREATE CONSTRAINT` over its data,
still fail. Concurrent duplicate commits themselves stay possible: that is
unchanged and out of scope.

**`prop_ix` updates.** The two new updates in `set_property_node` and
`add_labels` also make the overlay equality seek (`ExecGraph::lookup_nodes`,
`overlay/seek.rs:40-56`) see nodes whose property or label changed after
their pair was built. A test that depended on the old gap was asserting a
miss.

**Related ADRs.**

- [ADR-0171](./0171-lazy-per-label-snapshot-probe.md) removes the per-statement label census. With this ADR, a batched
  write's validation and label accounting no longer scale with graph size.
- MATCH and MERGE equality seeks on a property covered only by a `unique` or
  `key` constraint still go through `lookup_nodes_at`'s range-only path. That
  is out of scope and noted for the ingest work.

**Tests.**

- **Counter.** Add a test-only thread-local `CONSTRAINT_INSPECTS` in
  `exec/mod.rs`, following the `HOP_EXPANDS` pattern (`exec/mod.rs:45-53`),
  with a `#[cfg(test)]` `take_constraint_inspects()`. It counts:
  - each re-validated entity read;
  - each probe candidate verified;
  - each entity read by a section 5 recheck;
  - each relationship of type T read by the section 4 scan.

  The lazy postings build is index maintenance and is not counted.
- **Scaling tests.** These are executor unit tests that call `crate::execute`
  on an `ExecGraph::write` view on the test thread. The counter is
  thread-local, and `cypherlite::Connection` runs queries on a worker thread.
  - **Node kinds.** Preload 1,000, then 10,000, `:P` nodes under `unique`,
    `key`, `exists`, `propertyType` and `nodeLabelExistence` constraints. Run
    one warm-up write, reset the counter, then run
    `UNWIND range(1, 100) AS i CREATE (:P {…})` creating valid nodes with
    fresh values. Assert the count is **equal at both sizes**.
  - **Relationship non-unique kinds.** Do the same for `exists`,
    `propertyType`, `sourceLabelExistence` and `targetLabelExistence`
    relationship constraints.
  - **Relationship `unique` / `key`.** Make every pre-existing relationship
    type T. Assert the count grows by exactly the 9,000 extra relationships:
    once per relationship, not once per touched relationship.
- **Violation tests.** Add one integration test per section 2 table row and
  trigger op, in `cypherlite/cypherlite/tests/`. Each runs a statement that
  violates through exactly that op and asserts today's exact error text.
  Add these as well:
  - positive controls;
  - a duplicate across statements of one explicit transaction;
  - a duplicate across `IN TRANSACTIONS` batches, for both `ON ERROR FAIL`
    and `CONTINUE`;
  - `1` vs `1.0` rejected; NaN and null-bearing composite tuples accepted;
  - delete-then-recreate of the same value in one statement accepted;
  - `SET` a value away, then create it, in one statement accepted;
  - `CREATE (:K {x: 5}), (:K), (:K {x: 5})` still reporting
    ``ConstraintValidationFailed: `k` requires properties``;
  - the write-skew change above, pinned on purpose.
- **Differential oracle.** Keep today's full-scan validator as a
  `#[cfg(test)]` oracle. A differential test runs a statement corpus that
  covers every row through both validators and asserts equal `Ok` results
  and equal error texts.
- **Suites.** The TCK, CIP-eval and the existing constraint tests stay
  green and unchanged.

## Later references

- [ADR-0439](./0439-commit-time-uniqueness.md) closes this ADR's Concurrency gap: the group-commit admission
  loop now rejects a slot introducing a constrained value that exists in
  committed state or in an earlier accepted slot of the same flush group
  (a maintained per-constraint tuple map), so the "two explicit
  transactions that each create `(:P {id: 77})` both commit" probe result
  no longer holds — exactly one commits and the loser receives
  `Error::Conflict`; the retried statement sees the winner and this ADR's
  statement-time validation raises the precise violation. The
  ids-only commit-conflict limitation now covers non-constrained
  write-skew only.
- [ADR-0173](./0173-overlay-owned-redo-log-and-scratch-index.md) replaces three implementation details without changing the
  algorithm. Drain-on-entry becomes validate-then-mark: the cursor advances
  only after the slice validated. A redo-funnel posting in
  `WriteScratch::push_redo` replaces the per-primitive `prop_ix` hooks.
  `all_node_ids` / `all_rel_ids` become snapshot minus deleted plus
  created, replacing the `binary_search` merge. It also replaces §2's
  "Unknown tags" rule: a stored kind tag `ConstraintType::from_str`
  rejects fails validation.
- [ADR-0174](./0174-committed-catalog-constraint-rechecks.md) adds the committed-catalog fingerprint: a constraint another
  session commits or changes after this scratch last validated joins the
  recheck set at the next validation, and the recheck and partner-search
  loops become cancellable.
- [ADR-0184](./0184-composite-key-smallest-bucket-probe.md) refines §3's node probe: a constraint with two or more
  properties probes the property whose committed-plus-scratch bucket is
  smallest (ties to the lowest index) and takes candidates from that same
  property on both sides. Verification and error selection are unchanged.
- [ADR-0185](./0185-relationship-equality-postings.md) replaces §4's walk over the relationships of the type with
  lazy committed relationship equality postings keyed `(type id, key
  id)`, plus a scratch-side twin, and probes each touched relationship
 tuple the way §3 probes a node, choosing the property per [ADR-0184](./0184-composite-key-smallest-bucket-probe.md). §5
  is unchanged.
