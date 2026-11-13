# ADR-0520: The spec harness owns the Cypher language corpora; the in-repo TCK gate retires

Status: Proposed

Claim: c1cd635a933fe5179538651e44871b47ce9850a3

## Context

ADR-0056 made the openCypher TCK the Cypher language contract and ADR-0006
vendored the pin: the TCK 2024.3 feature files lived in `testdata/tck`, the
cucumber-expanded scenario count was the compliance denominator, and a
regressed scenario failed CI. A Rust harness grew around that gate —
`testing/relate-tck`, a strict runner (structural path comparison,
name-keyed columns, typed error class/phase/detail matching, exhaustive
side-effect assertions, hard failure on unknown steps) with a 3892-id
expected-pass allowlist and a skip list that only shrank — beside
`testing/relate-cip-eval`, the CIP-derived stretch scoreboard ratcheted by
`baselines/cip-eval.json`, and the engine's own TCK extension corpus.

Keeping the pin honest was a recurring cost, not a one-off: the four 2024.3
ORDER BY expectations Neo4j's Cypher 25 reverses were re-pinned (ADR-0458,
ADR-0472), the superseded `toString()` outline was removed from the
pin itself (ADR-0188), three fixture setups were re-pinned when Cypher 25
made a same-clause created-entity reference illegal (ADR-0513), and the
allowlist was regenerated with every change. The pin is a snapshot of
Neo4j's suite, so it falls further behind the dialect the engine targets
with each Cypher release.

ADR-0482 then added the spec harness:
`neo4j-labs/cypherlite-cypher-test-harness` runs Neo4j's own Cypher spec
suites — TCK, acceptance, cypher25, the converted correctness corpus —
against the engine through the **Python binding**, with the wheel built
from the PR's commit as the engine under test. Its `spec-harness` lane
reports coverage on every ready PR and never gates it (§4:
reporting-only, fixed traffic-light bands, no baseline or ratchet). The
engine therefore had two homes for one language signal, and the harness is
the one that measures the dialect Neo4j ships and the surface clients use.

The corpora moved to the harness on 2026-11-05:
`labs-1049-move-cypher-tests@2c1002de5901b1c644d5aa3e19c8cd63e810bf08`
transfers `testdata/relate-tck/*.feature` → `features/relate-tck/` (11
files / 489 scenarios), `testdata/cip-eval/CIP-*` → `features/cip-eval/`
(139 directories / 501 feature files / 5917 scenarios), and
`testdata/cip-docs-md/*.md` → `cip-docs-md/` (43 CIP
documents). The file sets are exact in both directions, and no scenario
step, table or expectation changed — only the behave-parse normalization
the harness already applies to its synced suites.

## Decision

1. **The corpora live in the harness.** `features/relate-tck/` and
   `features/cip-eval/` are harness-owned suites. New TCK extension
   scenarios and new CIP-derived scenarios are added there, not in this
   repository.

2. **The in-repo harness retires.** `testing/relate-tck` and
   `testing/relate-cip-eval` leave the workspace with the vendored corpora
   they ran, their 3892-id allowlist, the shrink-only skip list, the
   cucumber-expanded denominator, the `TCK_ONLY` / `TCK_DUMP_PASSING`
   knobs, `baselines/cip-eval.json`, and the `make test-tck` /
   `make cip-eval` entry points and CI steps.

3. **The 3892-of-3892 pinned-2024.3 claim is retired.** No engine document
   publishes a current TCK pass count or a "TCK compliant" claim any more. The
   language floor is still the openCypher TCK (ADR-0056), measured by the
   harness's `tck` suite — the upstream suite synced from Neo4j's
   compatibility-spec-suite — so the measured corpus is the current
   upstream TCK, not a vendored 2024.3 snapshot. An engine/TCK divergence
   is a harness report line, never an in-repo allowlist or re-pin edit.

4. **Reporting-only, and no relocated ratchet.** The two suites join the
   ADR-0482 §4 posture unchanged: the lane reports coverage with the fixed
   traffic-light bands and exits 0 in every band. No `baselines/*.json`,
   re-freeze workflow, skip list or threshold gate moves to the harness
   with them (user decision, 2026-11-05).

5. **The landing run's report is the record.** The transfer run (engine
   `f26e1c4d0`, `relate-db 0.16.1` wheel) reports:

   | Suite | Scenarios | Passed | Failed | Skipped | Expected-fail | Left failing |
   | --- | --- | --- | --- | --- | --- | --- |
   | `relate-tck` | 489 | 448 | 0 | 0 | 41 | 0 |
   | `cip-eval` | 5917 | 4942 | 924 | 51 | 0 | 924 |

   - `relate-tck` is the engine-green corpus (489/489 in the retired
     runner). Its 41 expected fails are harness-side contract gaps, marked
     per outline row in `features/expected_fail_list.txt`: 36 where the
     corpus labels Neo4j's error class (`ArgumentError`, `SyntaxError`)
     while the Python binding exposes only the coarse TCK class
     (`TypeError`) and carries the cause in the message — the retired Rust
     runner matched the labelled detail first — and 5 where a Gherkin `\n`
     cell escape cannot survive behave's substitution.
   - `cip-eval` is the stretch tracker; its failures are untagged and left
     failing on purpose. The retired suite was deliberately lenient (any
     error satisfied a `should be raised` step, unknown steps were
     skipped); the harness enforces the shared TCK grammar, so the same
     5917 scenarios report 924 left failing, classified by the report's
     failure categories (511 `setup-failed-syntax`, 256
     `error-class-mismatch`, 49 `side-effects`, 41 `result-vector`, 35
     `result-temporal-fidelity`, and smaller buckets). The 51 skips are
     the existing admin/RBAC policy: the embedded engine does not execute
     `SHOW USERS/ROLES/TRANSACTIONS` or `CREATE/DROP DATABASE`.
   - The same landing run reports the harness's synced `tck` suite at 6833
     passed / 25 failed / 7 Neo4j-expected-fail / 87 skipped: that is the
     openCypher TCK measurement that replaces the retired pin.

6. **Documentation records the retirement.** `docs/tck/compliance.md`
   becomes a pointer at the harness with the retired pin kept as clearly
   marked history; `AGENTS.md` names the spec harness as the
   language-contract home and loses the allowlist-editing recipe; ADR-0056
   gains a Later-references note.

## Alternatives considered

- **Keep the in-repo gate and the harness as independent signals.**
  Rejected: two homes for one corpus, and the pin lags the dialect — every
  Neo4j semantic change re-pays a re-pin ADR plus an allowlist
  regeneration (ADR-0188, ADR-0458, ADR-0472 and ADR-0513 are that cost
  already paid), while the two runners disagree on the same scenarios by
  construction (strict in-process Rust versus the Python surface).

- **Move the corpora, then re-establish a per-PR ratchet on them in the
  harness** (frozen baselines plus a re-freeze dispatch, the cip-eval and
  loadtest-envelope precedent). Rejected on the user decision of
  2026-11-05: ADR-0482 §4's reporting-only posture stands — a ratchet
  against the current upstream corpus would go red on Neo4j's own suite
  churn, and freezing it re-creates the pin problem one repository over.

- **Keep the in-repo copy as a diagnostic fallback while the harness owns
  the canonical corpus.** Rejected: a second copy drifts silently (the
  transfer alone needed 32 files normalized for behave's parser; the two
  copies would diverge the same way), and a fallback no CI runs is not a
  measurement.

- **Delete the engine corpora outright instead of moving them.**
  Rejected: `relate-tck`'s 489 scenarios cover engine-owned semantics the
  upstream TCK cannot — aggregate-then-slice postfix expressions, the
  `inf`/`nan`/`infinity` keyword resolution — and `cip-eval` is the
  proposal tracker; deleting them loses the signal instead of
  moving it.

- **Measure the floor against the retired pin inside the harness.**
  Rejected: the harness exists to measure the dialect Neo4j ships, from
  Neo4j's own suites (ADR-0482); vendoring 2024.3 there would move the same
  drift into the harness instead of retiring it.

## Consequences

- The engine loses its language merge gate: no in-repo run fails a PR for
  a TCK or CIP regression. The `spec-harness` lane reports it (job summary
  and PR comment) and exits 0 in every band — the deliberate ADR-0482 §4
  trade, now the only language measurement.
- `docs/tck/compliance.md` no longer publishes a pass count; it points at
  the harness and keeps the retired pin's history.
- A new CIP's scenarios, a new TCK extension, or a re-pin of
  a scenario are harness-side changes; the engine's ADR trail no longer
  records each re-pin.
- The 41 `relate-tck` expected fails are harness contract gaps — closing
  them (error detail across the Python boundary, Gherkin cell-escape
  handling) is harness work, and the corpus stays engine-green meanwhile.
- The 924 `cip-eval` left-failing scenarios are the stretch worklist; the
  report's failure categories are its triage, and no count gates the
  engine.
- `AGENTS.md`, `README.md`, `docs/limitations.md`, the CLI skill bundle and
  the expand-workload sources name the harness as the corpora's home; no
  engine doc claims 3892 of 3892 any more.
- If the harness is unreachable (missing read token, repo access lost), the
  engine still builds and ships; language coverage is simply unreported —
  there is no in-tree fallback measurement.
