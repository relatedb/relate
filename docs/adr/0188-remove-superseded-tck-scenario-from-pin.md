# 0188. Remove the superseded toString() TCK scenario from the pinned corpus

- **Status**: Accepted
- **Date**: 2026-10-08
- **Deciders**: CypherLite maintainers

## Context

The pinned openCypher TCK corpus (2024.3, ADR-0056/ADR-0006, vendored under
(`testdata/tck/`) contains one scenario whose expectation the 2026.09
display-form change deliberately reversed:

```gherkin
Scenario Outline: [10] Fail `toString()` on invalid types #Example: <exampleName>
  ...
  Then a TypeError should be raised at runtime: InvalidArgumentValue

  Examples:
    | invalid | exampleName  |
    | []      | list          |
    | {}      | map           |
    | n       | node          |
    | r       | relationship |
    | p       | path          |
```

(`testdata/tck/expressions/typeConversion/TypeConversion4.feature`.)

The outline expects `toString()` to raise a `TypeError` on
LIST/MAP/NODE/RELATIONSHIP/PATH. The display-form change — implemented in
Neo4j 2026.09, verified live under `CYPHER 25` — made `toString()` render
those types in display form instead. CypherLite implements the display-form
semantics (ADR-0002 records the authority ordering: a live-verified Neo4j
behavior is authoritative over the 2024.3 TCK floor), and the cip-eval
display-form suite carries the current contract, including the
`LIST<ANY>`-only acceptance rule under which the homogeneous lists of these
examples still error.

The scenario was therefore carried as the skip list's single entry
(`runner::skip_reason`), collapsed into one counted skip record for its five
Examples rows. The public claim read "3892 of 3893 pass, 1 documented skip",
and the skip list "only shrinks" (ADR-0006) — but this entry could never
shrink away: it does not encode an engine limitation that work can remove;
it encodes *pre-display-form semantics* the dialect has deliberately
outgrown.
Every future conformance claim would carry a permanent "1 skip" that no
engine change can ever legitimately fix.

## Decision

Remove the outline from the vendored pin itself, keeping it in the feature
file as a `#`-commented reference with the removal reason inline
(`TypeConversion4.feature`). The skip list becomes empty; the counted-and-
reported skip machinery (`skip_reason`, `EXPECTED_TCK_SKIPS`, the exact-set
skip gate) stays armed for any future skip. The claim becomes **3892 of
3892 pinned scenarios pass, 0 skips** — a 100% pass of the pin — with the
pin explicitly described as "2024.3 minus one superseded outline"
rather than unmodified upstream 2024.3.

## Alternatives considered

- **Keep the skip forever.** Rejected: the entry is permanent by nature (a
  reversed expectation, not a missing capability), so it would sit in every
  future claim and invite exactly the "just make it pass" regression that
  re-introducing the older `toString()` error would be.
- **Fix it by reverting `toString()` to error on those types.** Rejected:
  a deliberate regression of a Cypher 25 semantic change that Neo4j
  itself made (verified live) — the wrong direction for a
  Neo4j-parity dialect.
- **Re-pin the corpus to a newer upstream TCK that encodes Cypher 25
  semantics.** Rejected for now: a full re-pin is a large conformance surface
  change (new scenario inventory, re-baselined allowlist, possibly new
  gaps) that deserves its own effort; it does not block removing one
  scenario that both sides agree is superseded. Revisit when the upstream
  TCK catches up with Cypher 25.

## Consequences

- The denominator drops 3893 → 3892 (the outline's five Examples rows), the
  skip count drops 1 → 0, and the passing count stays 3892: the allowlist is
  unchanged (the skipped scenario was never on it).
- The TCK gate (`tck_integration.rs`) asserts the new numbers, an empty
  expected-skip list, and — via the rewritten runner test — that the
  removed outline stays removed, so a future re-vendor of the upstream
  feature file cannot silently resurrect it as a failing (or passing!)
  scenario without a conscious decision.
- The claim's wording changes from "not a 100% claim" to "100% of the
  pinned corpus", with the pin's composition stated explicitly wherever
  the claim appears (`docs/tck/compliance.md`, `docs/limitations.md`,
  README, AGENTS.md). The claim remains against *our* pin, not blanket
  upstream-2024.3 parity.
- Later references: ADR-0002 (display-form authority ordering), ADR-0006
  (skip-list policy — the list reached its natural empty end state, and the
  shrink-only rule is upheld), ADR-0056 (the 2024.3 pin itself, now with a
  documented local deviation).
