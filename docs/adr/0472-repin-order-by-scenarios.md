# ADR-0472: Re-pin the four Cypher 25-superseded ORDER BY scenarios to Neo4j behaviour

Status: Proposed

Claim: d7925c52d7df9ccb2fee13a752e6fd9af053fa96

## Context

ADR-0458 §12 removed four ORDER BY scenarios from the vendored openCypher
2024.3 pin because Neo4j's current Cypher 25 behaviour reverses their
expectations: `WithOrderBy4 [13]`, `[14]`, `[20]` and `ReturnOrderBy6 [5]`.
The engine implements the Cypher 25 behaviour — ADR-0453's re-aggregation
rule for ORDER BY, ADR-0458 §6's ORDER BY projection scope, and the
unaliased-`WITH`-item rejection — but the corpus edits did not agree on one
story: `[13]`/`[14]` were later re-added to the pin as expected-to-pass yet
red, while `[20]` and `ReturnOrderBy6 [5]` stayed commented out. The gate
therefore shipped at `TCK_PASSING` 3888 against a 3890 denominator and
`cargo test -p cypherlite-tck` failed on exactly those two ids by design.

A permanently red gate is not a gate: it cannot separate a real regression
from the known divergence, and it forces every public surface to state a
weaker claim ("3888 of 3890") than the engine's actual
behaviour supports. ADR-0002's authority ordering settles which behaviour
the pinned scenario should encode — a current, verified Neo4j behaviour
outranks the 2024.3 TCK floor. The acceptance suites already carry the
current forms: `WithOrderBy4 [13]`/`[14]` (success) beside their
`[13-c5]`/`[14-c5]` twins expecting 42I24, `[20]` expecting 42N21, and
`ReturnOrderBy6 [5]` ("This works in LTS") expecting an empty result set.

## Decision

All four scenarios stay in the vendored pin and assert Neo4j's current
Cypher 25 behaviour, allowlisted as expected-to-pass:

1. **`WithOrderBy4 [13]`** (`WITH a.num2 % 3 AS mod, min(sum) AS min ORDER
   BY sum(sum) LIMIT 2`) asserts success with the rows `(mod 2, min 7)` and
   `(mod 1, min 5)`. The ORDER BY aggregation recomputes over the group's
   input rows — the summed `sum` variable — so the groups order 7 < 13 < 15
   and `LIMIT 2` keeps `mod` 2 then 1.
2. **`WithOrderBy4 [14]`** asserts the same two rows for the non-projected
   `sum(a.num + a.num2)` spelling: same group sums, same ordering.
3. **`WithOrderBy4 [20]`** asserts `NoExpressionAlias` (42N21): the
   unaliased `WITH` item is rejected before any aggregation-ambiguity
   verdict, which is what the acceptance file's `[20]` pins, not
   `AmbiguousAggregationExpression`.
4. **`ReturnOrderBy6 [5]`** asserts an empty result set with columns
   `me.age + you.age` and `cnt`: an ORDER BY key that decomposes a
   projected expression beside an aggregation is accepted, and the empty
   graph yields no rows.

The pinned denominator and passing count both return to **3892**, the skip
list stays empty, and the conformance gate is green
(`passed == 3892 && failed == 0 && skipped == 0 && attempted == 3892`) with
the allowlist gate at exact set equality.

## Alternatives considered

- **Restore the 2024.3 expectations and reject the Cypher 25 forms.**
  Rejected: the acceptance suites tag `[13]`/`[14]` `@fails:cypher-5`,
  i.e. the 2024.3 verdicts are the ones Neo4j itself now fails; reverting
  would regress the dialect to pre-Cypher-25 ORDER BY semantics that ADR-0453
  and ADR-0458 §6 deliberately adopted.
- **Keep the scenarios out of the pin (ADR-0458 as written).** Rejected:
  excluding them hid a live divergence from the pinned floor, and the
  removed count no longer matched the engine's behaviour, so the claim
  understated conformance and gave a future regression nowhere to show up.
- **Keep `[13]`/`[14]` in the pin and failing.** Rejected: a gate that
  fails on known-good behaviour trains readers to ignore its red state, and
  it prohibits any future "zero failures" assertion.
- **Move the four scenarios to the skip list.** Rejected: the skip list
  only shrinks (ADR-0006) and a skip records a capability gap, not a
  superseded expectation; the engine passes these scenarios.

## Consequences

- The public claim becomes one statement: **3892 of 3892** pinned,
  cucumber-expanded scenarios pass, with 0 skips and no red ids
  (`AGENTS.md`, `README.md`, `docs/tck/compliance.md`,
  `docs/limitations.md`, `docs/reference/cypher-syntax.md`, the bundled CLI
  skill, and the `tck_integration.rs` constants and doc comments).
- The allowlist gains `WithOrderBy4::[20]` and `ReturnOrderBy6::[5]`;
  `[13]`/`[14]` keep their existing entries.
- ADR-0453's "awaits explicit human approval" consequence bullet and
  ADR-0458 §12 and its Consequences bullet are superseded by this decision;
  both carry a Later references note.
- The 2024.3 pin is still not unmodified upstream: one superseded
  outline remains removed (ADR-0188), and these four scenarios encode
  Cypher 25 expectations rather than 2024.3 ones.
