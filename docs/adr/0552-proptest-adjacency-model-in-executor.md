# ADR-0552: Proptest adjacency reference model in relate-executor

Status: Proposed
Claim: f78d5d1ddf58b3cc63ccab422598f554c2db3378

## Context

An earlier decision adopted `proptest` as a dev-dependency of the storage
crate only, with the clause that a later need in a different crate gets
its own ADR. That need is here: the sealed+delta store (ADR-0551) changes
the physical layout of per-node adjacency (typed adjacency, ADR-0031 and
ADR-0033), and the rework needs a red-line guard that proves behavioural
equivalence — a property suite pinning the observable adjacency (per
(node, type, direction) iteration order and contents: missing-`ts` first,
`ts` DESC, newest-relationship-first ties per ADR-0035 and ADR-0203)
against a plain `BTreeMap` reference model under random create / ts-update
/ delete / re-add sequences.

The suite must exercise the executor's real store structures (typed
adjacency and the graph's adjacency maps), which are crate-internal, so it
lives as a test module inside `relate-executor` and needs `proptest` as a
dev-dependency there. The design was completed in the project's pre-rename
home and is ported here for the record.

## Decision

1. **Adopt `proptest` 1.11 as a dev-dependency of `relate-executor`**, at
   the storage crate's pin. No `proptest-state-machine`: the suite drives a
   plain generated vector of operations, like the other storage suites.
2. **Boundary ruling.** The workspace dependency rules of ADR-0103
   (R1–R5, enforced by the boundary check) govern workspace-internal
   edges by kind; external dev-dependencies are not edges the checker
   evaluates, and proptest's tree has no native/C++ build, async runtime,
   or networking, so the heavy-dependency trigger does not apply. The
   placement is boundary-clean, and the suite reaches the crate-internal
   structures it guards — the home an external test could not provide.
3. **Observable-semantics guard.** The property compares observable
   adjacency (visibility-filtered iteration), not physical hop vectors:
   deletes are MVCC tombstones that never touch adjacency today, and the
   rework may change those internals while this suite stays green.
4. The standing rules of the storage-only adoption apply unchanged in this
   crate: 256-case default with `PROPTEST_CASES` override (no pinned
   case count), committed regression files, and the rand 0.8 / rand 0.9
   dual-major separation (no test mixes the two RNG families).

## Alternatives considered

- **Integration test driving the public `Database` API.** Boundary-safe
  without this ADR, but the public API cannot observe per (node, type,
  direction) chunk order or the newest-first tie-break directly: `MATCH`
  row order is not specified to expose adjacency order without `ORDER BY`,
  so the guard would pin less than the rework must preserve or over-fit
  incidental planner behaviour. Rejected: the guard must see the store
  structures.
- **Suite in the shared test-support crate.** Same reachability problem in
  reverse: it may depend on engine crates, but the typed adjacency and its
  maps are crate-internal and unreachable from outside. Rejected.
- **`proptest-state-machine`, like the WAL state-machine suite.** Its
  reference-state-machine shape fits commit transitions; the adjacency
  alphabet (create / ts-update / delete) with an after-every-op full-state
  assertion is a generated operation sequence more directly, matching the
  other storage suites. Rejected: an extra dependency for no extra
  assurance.
- **Extend the workspace-wide proptest adoption.** The storage-only
  decision rejected that as a starting point; this ADR adds exactly one
  crate with a concrete property, not a blanket adoption. Rejected:
  unchanged rationale.

## Consequences

- `relate-executor` gains a dev-dependency on `proptest` 1.11; the
  storage crate's "storage-only" comment is corrected to reference this
  ADR, and the earlier storage-only decision gains a later-reference note.
- No production build, published crate, or release artifact changes;
  `Cargo.lock` already carries the proptest tree, so no second copy.
- The adjacency model suite becomes part of the `cargo test -p
  relate-executor` merge gate and scales through `PROPTEST_CASES`.
- Snapshot-freeze and compaction-confluence properties are out of scope
  here: the current store has no compaction to write them against. They
  are stubbed as documented ignored tests and land with ADR-0551.
