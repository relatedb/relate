# OpenCypher TCK compliance

The openCypher TCK is still the language contract (ADR-0056), but it is no
longer measured inside this repository. The **spec harness**
(`relatedb/relate-cypher-compat-private`, ADR-0482) runs Neo4j's Cypher
spec suites — the openCypher TCK synced from Neo4j's compatibility-spec-suite
among them — against the engine through the Python binding: the wheel built
from the PR's commit is the engine under test. The `spec-harness` lane
(`.github/workflows/spec-harness.yml`) reports coverage on every
ready-for-review PR with a traffic light in the job summary and a PR comment,
and on every merge to main (the run at the merge commit), and never gates the
build on it.

The engine's own TCK extension corpus and its CIP-derived evaluation suite
moved to that harness on 2026-11-05 (ADR-0520). The in-repo Rust harness, its
3892-id allowlist, its skip list, its cucumber-expanded denominator and every
pass claim retired with them: no engine document publishes a current TCK pass
count any more.

## Historical record: the in-repo pin (retired 2026-11-05)

Until the move, the engine vendored openCypher TCK **2024.3** under
`testdata/tck/` and ran it through `testing/relate-tck`, a strict Rust runner
(structural path comparison, name-keyed columns, typed error class/phase/detail
matching, exhaustive side-effect assertions, hard failure on unknown steps).
The cucumber-expanded scenario count was the compliance denominator
(ADR-0006); `testing/relate-tck/tests/tck_integration.rs`
(`test_tck_full_summary`) asserted `passed == 3892 && failed == 0 &&
skipped == 0 && attempted + skipped == 3892 && lenient_error_fallbacks == 0 &&
passing_ids == allowlist` in CI via `cargo test -p relate-tck`.

| Metric | Value at retirement |
| --- | --- |
| Pin | openCypher TCK 2024.3, with the deviations below |
| Expanded scenarios | 3892 |
| Passing (allowlist, exact set equality) | 3892 |
| Explicit skips | 0 |
| Pass rate (of attempted) | 100% |

The claim was made against that pinned corpus, not as blanket parity with
unmodified upstream 2024.3 — one superseded outline was removed from the pin
and seven scenarios were re-pinned to current Neo4j behaviour:

| Scenario | Deviation from unmodified 2024.3 |
| --- | --- |
| `TypeConversion4 [10]` — *Fail `toString()` on invalid types* | Removed from the pin: CIP-271 (ADR-0002) renders LIST/MAP/NODE/RELATIONSHIP/PATH instead of erroring, so the outline can never pass without regressing (ADR-0188). |
| `WithOrderBy4 [13]`, `[14]` — *Fail on sorting by a non-projected aggregation …* | Re-pinned to Cypher 25's recompute-over-the-group behaviour (ADR-0458, ADR-0472). |
| `WithOrderBy4 [20]` — *… complex expressions inside an order by item with an aggregation* | Re-pinned to the unaliased-`WITH`-item error 42N21 (ADR-0458, ADR-0472). |
| `ReturnOrderBy6 [5]` — *… complex returned expressions inside an order by item with an aggregation* | Re-pinned to Cypher 25's acceptance of the decomposed ORDER BY key (ADR-0458, ADR-0472). |
| Three WITH/SKIP/LIMIT fixture setups | Re-pinned to separate `CREATE` clauses: Cypher 25 rejects a same-clause created-entity reference (42I58, ADR-0513). |

The skip list only shrank and ended empty (ADR-0006, ADR-0188). The
per-scenario rationale is recorded in ADR-0188, ADR-0458, ADR-0472 and
ADR-0513. The engine's own extension corpus and CIP-derived suite moved to
the harness (ADR-0520); its `features/expected_fail_list.txt` and per-CIP
`NOTES.md` carry their markings.
