---
type: adr
area: cypher
claim: cf544a91e78c10efdd956258ec750688c9921d47
date: 2026-11-25
deciders: Relate maintainers
name: 0688-tostring-renders-every-list
status: proposed
tags:
  - conformance
  - functions
  - semantics
title: toString() renders every list value — the static homogeneous-LIST<T> rejection retires
REFERENCES:
  - "[[adr/0002-cypher-25-semantic-target]]"
  - "[[adr/0056-opencypher-tck-language-contract]]"
  - "[[adr/0188-remove-superseded-tck-scenario-from-pin]]"
  - "[[adr/0327-engine-statement-scoped-bytes-value]]"
  - "[[adr/0371-preview-feature-gating]]"
  - "[[adr/0448-tostring-temporal-full-precision]]"
  - "[[adr/0520-move-cypher-tests-to-spec-harness]]"
  - "[[adr/0584-one-cypher-version-cypher25-noop]]"
  - "[[adr/0661-spec-harness-regression-gate]]"
---

# 0688: toString() renders every list value — the static homogeneous-LIST<T> rejection retires

## Context

The display-form change (Neo4j 2026.09) made `toString()` render
containers and graph values instead of raising. Relate implemented it with a
boundary taken from a live probe: a *homogeneous* list literal — a statically
typed `LIST<T>` whose elements share one class (`[1, 2]`, `['a', 'b']`,
`[1, 2.0]`, `[[1]]`, `[{a: 1}]`; INTEGER and FLOAT are one numeric class) —
was refused at compile time with `InvalidArgumentValue: toString()`, while a
list without a statically known element type (a property, a parameter, a
`WITH`-bound variable) rendered. The front end owned the rule
(`check_tostring_argument` over `relate_core::homogeneous_element_class`);
the executor already rendered every list value it was given.

The synced language corpus's Cypher 25 semantics contradict the static
half: `toString()` renders *every* list value in display form, homogeneous
literals included — `toString([1, 2])` → `'[1, 2]'`, `toString(['a', 'b'])`
→ `'[a, b]'`, `toString([true, false])`, `toString([1.0, 2.0])`,
`toString([1, 2.5])`, `toString([[1, 2], [3]])`, `toString([{a: 1}, {b:
2}])` — where Cypher 5 raised on lists. Seven list-shaped scenarios fail
on engine main with the compile-phase rejection — honest gaps in the
sync drift's own classification; this ADR is the engine work that closes
them.

Two signals disagreed, and both are now explained. Live Neo4j **2026.09
and 2026.10.1**, under an explicit `CYPHER 25` preamble (and under
`CYPHER 5`), both still refuse `toString([1, 2])` — the Cypher 25
rejection message spells the accepted list form as `LIST<ANY>` and no
other. The explanation landed in the Neo4j codebase on **2026-11-11**,
after both of those builds were cut: a single change widened the
semantic type check so a `LIST<T>` argument matches the accepted
`LIST<ANY>` type, added the implementation's own unit tests pinning
Cypher-25-accepts / Cypher-5-rejects for every list type, and extended
the spec suite's rows to the same commitment. The re-probe measured
release builds that predate the fix; the synced corpus carries the fix.
What the boundary never was, is specified: the display-form feature's
own proposal document renders lists element-wise with no homogeneous
carve-out — its example table's list row is itself a homogeneous integer
list, proposed to render as `"[1, 2, 3]"` — and the boundary entered
this engine's records only as a probe of the first implementation's
type-checker strictness, a defect upstream has now fixed, not semantics
anyone committed to.

The maintainers resolve this case for the corpus, which is also the
current codebase: the engine follows the committed Cypher 25 semantics.
[ADR-0002](./0002-cypher-25-semantic-target.md)'s authority ordering —
live over doc — was written to stop the engine implementing behavior
*no real system exhibits and no contract commits to*; here the codebase
exhibits it, the contract commits to it, and only two stale release
builds lag. The engine's dialect is `CYPHER 25` only
([ADR-0584](./0584-one-cypher-version-cypher25-noop.md)); the harness
owns the language corpora
([ADR-0520](./0520-move-cypher-tests-to-spec-harness.md)) and its synced
corpus is the floor's current upstream expression
([ADR-0056](./0056-opencypher-tck-language-contract.md)).

## Decision

`toString()` and `toStringOrNull()` render every `LIST` value in the
display form, whatever spelling produced it — a literal, a `WITH`-bound
variable, a property, a parameter, a function result alike. The rendering
policy is the value layer's, on the
[ADR-0448](./0448-tostring-temporal-full-precision.md) principle: a
value's string form never depends on how the value was spelled.

1. The front-end static check is deleted — the `tostring` arm,
   `check_tostring_argument`, `is_homogeneous_list_literal` — and with it
   the rule's one implementation, `relate_core::homogeneous_element_class`
   and `LiteralElementClass` (no caller survives). The vocabulary of
   "which lists `toString()` accepts" collapses to "every list".
2. The one value-level type error that stays is a byte array
   ([ADR-0327](./0327-engine-statement-scoped-bytes-value.md)):
   `toString(bytes)` raises, `toStringOrNull(bytes)` is null.
   `TOSTRING_TYPE_ERROR_DETAIL` stays, executor-owned.
3. Sibling converters are unchanged: `toInteger()`/`toFloat()`/
   `toBoolean()` raise on a list argument at the value level — the
   corpus's Cypher 25 semantics still expect those rejections — and
   their `*List()` forms keep converting element-wise.
4. The feature's stretch-suite copy retires from the harness repository
   rather than being corrected: a graduated (Implemented) change's
   contract is the synced suites', and a second home for the same facts
   is the drift trap this change's gate run exposed — the stretch copy
   pinned the probe-derived rejection the synced rows had already
   superseded. The retirement entry records the probe, the upstream
   fix, and the two pins the synced rows do not carry (map-key sorting,
   nested-map recursion), which stay engine-unit-pinned. Retiring the
   rest of the graduated corpus is recorded as its own card.

## Alternatives considered

- **Keep the static rule (follow live), carrying the seven scenarios as
  failing or expected-fail.** Rejected: the re-probe shows every current
  live build still rejects, so this was the live-first reading — but the
  boundary is a probe artifact of the first implementation's type
  checker, not semantics any contract states, and carrying it would
  enshrine the artifact while the committed corpus moves the other way.
  The maintainers resolve the authority question for the corpus in this
  case (Context above).
- **Move the homogeneous rule to the executor** (refuse every
  homogeneous list at runtime, literals included). Rejected: it trades
  seven failures for more — a property, parameter, and function-result
  list renders in the corpus, in the probe, and in the engine today; the
  rule has no correct home at the value layer.
- **Gate the rendering behind `RELATE_PREVIEW`.** Rejected:
  [ADR-0371](./0371-preview-feature-gating.md)'s registry is for preview
  features beyond the dialect floor; this change aligns with the
  floor's current upstream corpus and extends nothing.

## Consequences

- The seven scenarios pass; the
  [ADR-0661](./0661-spec-harness-regression-gate.md) ratchet sees no new
  failing id (all seven already fail on main and leave the failing set).
- Compile-phase behavior changes observably: `EXPLAIN RETURN
  toString([1, 2])` plans, and a never-evaluated `CASE` arm no longer
  fails the statement. The tests pinning the compile-time rejection are
  replaced by render pins, front-end and end-to-end.
- `docs/reference/functions.md` drops the homogeneous-list paragraph:
  every list renders in display form; a byte array stays the one
  refusal.
- [ADR-0188](./0188-remove-superseded-tck-scenario-from-pin.md)'s
  recorded residue — "the `LIST<ANY>`-only acceptance rule under which
  the homogeneous lists of these examples still error" — is superseded;
  a Later-references note on it points here.
- The probe-versus-corpus conflict is recorded above as settled *for the
  corpus*, which is the current codebase: the Neo4j change landed on
  2026-11-11, and the two release builds the re-probe measured simply
  predate it — the next release renders. The stretch copy of the
  feature retires with this change; the day a build carrying the change
  ships, the synced rows re-verify against it. If upstream ever
  reverts the change instead, the engine follows and this ADR is
  superseded by its own successor.
