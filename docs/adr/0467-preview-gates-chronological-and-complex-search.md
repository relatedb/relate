# ADR-0467: Two corpus-visible preview gates: chronological comparisons and complex SEARCH patterns

Status: Proposed

Claim: f5b3fd04abff664a96988c1ac9bce25e90d23284

## Context

Two Neo4j acceptance-corpus switches are modelled as deployable
configurations the engine does not yet represent. Both are ungated today,
so one side of each pair cannot be expressed. ADR-0464's
`float16-coordinate-types` row is the same shape, recorded by the same
round.

- **Chronological comparisons.** `TemporalChronologicalCompare` carries
  the suite's enabled `@conf` tag and its 43
  rows assert the four Cypher 25 operators' results.
  `TemporalChronologicalCompareDisabled` pins the off side: the
  `date(…) IS BEFORE date(…)` query in its only scenario must raise an
  error. Its error table carries two rows, which the harness reports as
  `metadata-unverified` and does not enforce beyond "an error was raised".
  The `temporal-literals` node added the operators ungated, so the disabled
  scenario now fails with "expected an error but the query succeeded".
- **Complex SEARCH patterns.** `FulltextSearchWithComplexPatternAcceptance`
  (17 rows) and `SearchWithComplexPatternAcceptance` carry
  the suites' complex-pattern `@conf` tag
  and assert results for SEARCH MATCH statements whose patterns are
  multi-component, multi-hop, variable-length, quantified, selected,
  multi-variable, or carry predicates outside the binding.
  `FulltextSearchAcceptance` scenarios 11-13 pin the off side: the
  detailed pattern-shape rejections. The front rejects the whole shape
  family unconditionally (`validate_search_pattern_shape`), so all 17
  acceptance rows are `class-syntax-error` — while the executor already
  handles every one of those shapes: with the shape check lifted, the 17
  rows pass unchanged.

## Decision

Register two rows in the ADR-0371 registry and gate each at the point its
off form is decided.

1. **`chronological-comparisons`** — mirroring
   Neo4j's own toggle. The four operators
   keep parsing into `Expr::ChronologicalCompare` (the parser recognizes
   them contextually after `IS`, so the floor grammar is untouched) and the
   front's AST-detector loop fails the statement closed with the authored
   `preview feature 'chronological-comparisons' is not enabled` error unless
   the row is enabled. Detector and gate are the ADR-0464 shape.

2. **`complex-search-patterns`** — mirroring
   Neo4j's own mode.
   The gate *widens validation* instead of detecting a construct: the front
   derives a `ValidateOptions` from the store's resolved preview set and
   threads it through `resolve_and_lower` → `lower_statement_with_options` →
   `validate_statement_tree_with_options` → `validate_search_clauses`. With
   the row off, `validate_search_pattern_shape` runs exactly as before and
   every pattern-shape rejection keeps its authored detail
   (`SearchClauseTooComplexPattern`, `SearchClauseMultipleBoundVariables`,
   `SearchClauseInvalidPredicates`); with it on, the shape family is
   skipped. Everything else the SEARCH rules check — the binding-variable
   reference, the per-index kind×option compatibility, the self-reference
   rules — holds in both states.

3. **Both rows cover every SEARCH source kind.** The pattern-shape family is
   kind-agnostic (the engine's pattern-shape check covered FULLTEXT
   exactly as VECTOR before this ADR), and Neo4j's setting is named for
   the search feature, not one index kind.

4. **One ADR, two rows.** The two switches are independent; they share the
   round, the precedent and nothing else.

## Alternatives considered

- **Gate `chronological-comparisons` at parse (a third `ParseOptions`
  flag that stops recognizing the operators).** Rejected: the off side's
  error class is not enforced by the corpus's two-row table, so ADR-0371's
  authored error satisfies it, and a parse gate would re-create the store's
  resolved preview knowledge inside the parser crate for no corpus gain.
- **Gate `complex-search-patterns` with an authored-error AST detector.**
  Rejected: the floor's off form is not "the construct is invisible to the
  parser" — it is those same detailed rejections, pinned by
  `FulltextSearchAcceptance` and the front's unit tests. Replacing
  them with the authored preview error would trade the corpus's ruled-out
  shapes for a different error contract; the gate must widen validation.
- **One registry row for both switches.** Rejected: a single
  `CYPHERLITE_PREVIEW` name cannot say which switch is meant, and enabling
  chronological operators for a complex-SEARCH user (or the reverse) is
  exactly the conflation the registry exists to avoid.
- **Leave both ungated and file the rows as harness skips.** Rejected: the
  corpus pins two real Neo4j configurations; the engine has the behavior on
  both sides (the executor already returns the acceptance results, the
  temporal operators already evaluate), so a skip would hide a modelled
  switch behind harness policy.

## Consequences

- The registry grows to 11 rows. The dialect floor is unchanged except for
  the chronological operators, which now fail closed ungated — the change
  `TemporalChronologicalCompareDisabled` exists to pin.
- `docs/reference/cypher-syntax.md` gains a chronological-comparisons
  subsection under the comparison operators and the SEARCH section names
  the pattern-shape rejections beside the new opt-in.
- Repo pins are four test binaries, split by gate state because
  `CYPHERLITE_PREVIEW` is process-global and read once per `Database::open`:
  `chronological_comparisons_gate.rs` / `chronological_comparisons_closed.rs`
  and `complex_search_patterns_gate.rs` / `complex_search_patterns_closed.rs`.
  The front crate pins both gates' entry-point behavior in unit tests.
- The acceptance harness needs two `features/environment.py` mappings that
  this ADR does not ship (harness PR #3):
  the corpus's chronological key → `chronological-comparisons`
  (a boolean key, the `CONF_TO_PREVIEW` table) and
  the corpus's complex-pattern key pair
  → `complex-search-patterns` (the valued `CONF_VALUE_TO_PREVIEW` table, like
  the existing mixed-label entry). Until then the
  enabled suites only pass with the flag exported for the run.
- Measured on this change: all 17 `FulltextSearchWithComplexPatternAcceptance`
  rows pass with `CYPHERLITE_PREVIEW=complex-search-patterns` and all 17 fail
  with the pattern-shape rejections without it; `FulltextSearchAcceptance`'s closed
  suite is unchanged and [11]-[13] pass; the `chronological-comparisons`
  enabled suite stays at its pre-existing 41 of 43 (the two failures are
  temporal-fidelity rows — an out-of-range year literal and a DST-gap named
  zone — that fail identically before and after), and the disabled scenario
  passes again.
