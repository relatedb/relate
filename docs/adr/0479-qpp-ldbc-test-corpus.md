# ADR-0479: The QPP/LDBC test corpus as query-latency workloads

Status: Proposed

Claim: 40f21a6d20282427e4f9fa243e8915e51ef17ac8

## Context

The openCypher TCK 2024.3 pin (3892/3892) carries **zero quantified path
pattern (QPP) scenarios** — verified 2026-10-31 — so the engine's QPP
support (path modes, quantified groups, the ADR-0468/0471/0475
traversal work) has no contract net beyond hand-written unit pins. The
2026-11-01 PathDB spike (LABS-646 pre-work) translated the 142 regular
path queries of dbgutalca/pathdb-eval1 (Apache-2.0, `e62e33c2`) into
Cypher QPP and ran them against the GBL SF0.1 store: 100 of 142 executed
(0.03-350 ms), 42 cyclic-recursion classes timed out under the watchdog,
and the run surfaced engine findings (the mode-prefix cliff, LIMIT
early-exit, walk budget, memory blow-up — since split into LABS-660/661,
closed where ADR-0471/0475 covered them) plus two correctness surprises
pinned separately in the engine tests.

Michael's scope directive for turning the spike into an asset: take ONLY
queries and substitution parameters from external corpora — never their
drivers, schedulers or reload machinery — and land them under the
existing LDBC test setup, not a new workload framework. Pre-work
(`corpus-manifest.md`, `license-notes.md`, `seed-policy.md`) curated the
file trees, checked every license, and recommended landing pathdb plus
the in-house fraud suite first, deferring SNB BI and ukrailway.

The spike driver itself was an uncommitted 676-line example
(`examples/pathdb.rs`) reading a TSV from `/tmp` — not a maintainable
lane.

## Decision

The corpus is workload directories inside
`testing/cypherlite-query-latency/workloads/` plus a first-class
`query-latency corpus` subcommand — extending the crate that already owns
the GBL lane (ADR-0243/0244/0245), with its data fetch, store reuse,
watchdog and report machinery reused, and no new framework:

- **`workloads/pathdb/`** — 142 translated queries + 65 path-mode
  restrictors over the same pinned GBL store (no new data rights), with
  a split seed layout: `seed-keys.json` is the one canonical
  `(seed label, first-hop types)` fact table (36 keys, one record per
  scale factor, each carrying the chosen node's id, degree and label;
  the key is canonical — the label plus the type set sorted and
  de-duplicated, so one fact has exactly one spelling, enforced on
  load), and `params-sf<SF>.json` holds only the derived per-query
  entries.
  PathDB's seed ids are LDBC datagen outputs that exist in none of our
  stores, so each is substituted by the **degree-median policy**
  (deterministic, representative, never a hub); the per-query entries
  are derived from the one fact table and checked against it on load
  (seed id and record label both).
- **`workloads/fraud-qpp/`** — six in-house re-authored queries (the
  external inspiration is unlicensed and was never copied): f01/f02 over
  the `cypherlite-datasets` fraud generator with its planted cycle as an
  engine-independent oracle, f06 over a designed sample fixture
  answer-checked exactly, f03-f05 kept until their generators exist.
- **`workloads/snb-bi/`** — added by the LABS-646 follow-up (2026-11-02),
  per the manifest §2 row deferred at this ADR's landing: the 16
  translated LDBC SNB BI analytics queries (ldbc/ldbc_snb_bi `47dd38b4`,
  Apache-2.0) over the same GBL store, with NO seed policy — upstream's
  parameters are literal values (dates, tags, windows), recorded per
  scale factor as one Cypher expression per parameter, which the lane
  evaluates once and binds natively (empty-answer params substituted
  same-kind and recorded, ADR-0247 precedent). The seven dialect forms
  the translation leans on were verified by execution before any file
  was marked `ok` — all supported; the workload README records the
  verdicts and the initial 11 `ok` / 5 `kept` split (the `kept` five are
  the unbounded `replyOf*0..` walk class, LABS-660).
- **`workloads/ukrailway/`** — added 2026-11-02, the manifest §5 cut
  order's last deferral: the two IN-HOUSE UK railway graph queries over
  the workload's OWN graph — the 18 MB dogofbrian/ukrailway data set
  (`ce82229e`, Apache-2.0) fetched on demand from the pinned commit,
  verified per-file against `data.sha256` (never committed) and
  imported into `:memory:` by the workload's `import.cypher` (LOAD CSV
  confined to the fetched directory) — no GBL store, no parameters (the
  queries carry their constants inline), no seed policy, no scale
  factor. r02/r03 were unverified drafts at this ADR's landing; both
  query texts and both answers were verified by execution against the
  pinned data before any file was marked `ok` (r02 1248 routePairs /
  19 changeoverStations, r03 15 hops and the station list — recorded in
  `oracle.json` as rows checks). r02 is minutes-scale (213.8 s measured
  in the lane), so the lane carries its own 600 s per-entry timeout,
  never the scale-factor-scaled default.

The **file format** contract: one statement per file, `$seed` parameters
(the engine's native parameter binding — ADR-0247's literal substitution
stays GBL-only), and exactly one machine-read `// status:` marker line —
`ok` (execute, must succeed: the ok-set ratchet, which only shrinks with
a tracked reason like the TCK allowlist), `kept` (parse + EXPLAIN only,
never execute: the frontier-NFA acceptance cases, LABS-644), or
`error-pin <ErrorName>` (must fail with that authored error). The marker
IS the ok-set — one fact, read by the runner, edited only with a reason.
Headers otherwise carry provenance (upstream repo + pin, Apache-2.0).

**Execution policy**: run-and-report only (ADR-0289 memory-shape
precedent) — no corpus number enters a frozen gate compare set and no
latency gates a PR; what gates is the ratchet, the error pins and the
oracle checks where an oracle is engine-independent. Timeout 10 s per
entry at SF0.1 (20 s at SF1), watchdog-cancelled; a previously-`ok` entry
newly timing out IS a failure. Seed regeneration rides the same run:
`--seed-policy median` (the default) re-resolves every key and reports
the diffs — an expected outcome of a re-frozen data set, never a failure.

Tests: unit (marker parsing, seed invariant, ratchet matrix, oracle
shapes, degree-median policy), a normal-lane fraud run (fast, no data
set), a parse/plan net over every corpus file with no data, and gated SF0.1
and SF1 pathdb runs behind the lane's existing `CYPHERLITE_QUERY_LATENCY`
env precedent (the snb-bi and ukrailway follow-ups ride the same gates).
SNB BI landed as the first follow-up (2026-11-02); ukrailway landed the
same day as the second — its r02/r03 drafts verified by execution against
the pinned data (r02 1248/19, r03 15 hops) before any file was marked
`ok`, exactly the gate this ADR demanded of them.

The two spike correctness surprises land as engine pins in
`cypherlite/tests/labs646_corpus_engine_pins.rs`: `count(p)` over a
fixed-length path raising `UndefinedVariable` (a defect, carried by
LABS-646, pin flips on fix), and per-branch UNION LIMIT composing
additively past any single cap (correct Cypher that the corpus's
`(A? | B)` translations rely on — 115 rows for two LIMIT-100 branches).

## Alternatives considered

- **A new `cypherlite-qpp-corpus` crate.** Rejected: a second workload
  framework beside the GBL lane duplicates fetch, load, watchdog and
  report code, and violates the scope directive ("no new top-level
  workload framework, no LDBC-driver code").
- **Take the corpora's drivers too (PathDB's jars, LDBC's paramgen).**
  Rejected: the directive excludes external evaluation machinery, the
  drivers pull heavy dependency trees (R6 territory), and the jars are
  unnecessary — the queries and a seed policy reproduce the workload.
- **Vendor the PathDB `Queries.xlsx` and translate at run time.**
  Rejected: the pin would carry an opaque binary artifact and the
  translation would be code; committed `.cypher` files with provenance
  headers are reviewable, diffable and greppable.
- **Run the PathDB seeds verbatim against the GBL store.** Rejected:
  0 of 26 original seed ids exist in any GBL store (different datagen
  run) — every query would match nothing; a recorded substitution policy
  is required for the corpus to mean anything.
- **Freeze row counts as gate baselines now.** Rejected: row counts are
  engine-self oracles (the Neo4j-verified answers are follow-up work);
  freezing them would gate PRs on the engine's own output.
  Run-and-report with the status ratchet is the honest first cut.
- **A separate `ok-set.json` ratchet file beside the headers.**
  Rejected: two places stating one fact (the header marker and the json)
  is parallel state; the marker is the single source the runner parses
  and the reader sees.
- **`--seed-policy orig` as well (the spike's diagnostic mode).**
  Rejected: it needs the original seed ids, which live only in header
  prose since the TSV was not committed; a diagnostic that re-mines
  prose is not worth the stringly surface.

## Consequences

- The engine's QPP support gains a 213-file contract net that the TCK
  cannot give, with a ratchet that catches both dialect regressions
  (the parse/plan net runs with no data) and execution regressions (the
  gated SF0.1 run) — the acceptance net for the frontier-NFA kernel
  (LABS-644): each `kept` entry becomes `ok` only by running and
  passing, never by reclassification alone.
- **Seed layout:** the workload's one seed-key fact table lives in
  `workloads/pathdb/seed-keys.json` (36 keys, both SF records, each
  carrying the resolved node's label — checked against the key's label
  on load, so a key can never silently name a node of another label);
  the per-SF `params-sf<SF>.json` files carry only the derived
  per-query entries. The split keeps the fact table existing exactly
  once and every file under the 1k-line split rule (the first cut
  carried one 1219-line params file under a size waiver; the split
  replaced the waiver).
- `cypherlite-query-latency` gains a normal dependency on
  `cypherlite-datasets` (R4-legal, testing group) for the fraud
  generator; the fraud lane's data and oracle are one deterministic
  generator call.
- The cyclic and unbounded classes stay visible as `kept` entries with
  their LABS references — the corpus is the scoreboard for the frontier
  kernel, not a silent skip list. (Ten restrictor entries first recorded
  as `kept` with copied recursion reasons were re-measured and
  reclassified to `ok` the day the corpus landed: r021-r025/r056 are
  bounded two-/three-hop shapes the unbounded-recursion reason could not
  apply to, and r006/r007/r009/r010 walk the acyclic replyOf forest,
  which the cyclic-recursion reason cannot apply to — the unbounded
  quantifier under the mode prefix terminates in ~86 ms.)
- A GBL data re-freeze regenerates every pathdb seed: run
  `corpus --seed-policy median`, review the reported diffs, re-record
  the params file (the manifest's documented loop).
- SNB BI and ukrailway remain unlanded (their own PRs, manifest §5);
  the ukrailway r02/r03 oracle drafts must reproduce the spike's
  numbers before either lands. (Both have since landed — the 2026-11-02
  follow-ups recorded above; the deferred-workload list is now empty.)
- The `count(p)` pin fails loudly when that defect is fixed — the fix's
  author updates the pin and the header note, by design.
