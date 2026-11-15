# pathdb — PathDB RPQ corpus (LABS-646, ADR-0479)

142 translated regular-path queries plus a 65-restrictor path-mode
matrix, running over the same pinned graph-benchmark-ldbc store as the `graph-benchmark-ldbc` lane
(`workloads/graph-benchmark-ldbc/`, ADR-0243 — no new data).

## Provenance and license

- Source: [dbgutalca/pathdb-eval1](https://github.com/dbgutalca/pathdb-eval1)
  @ `e62e33c2a40aee3cecca35961dd6da94a575c4f1` ("Upload compiled jars of
  baseline and PathDB", 2026-03-31), **Apache-2.0** (verified in a local
  clone). Paper: arXiv 2507.01755.
- What was taken: ONLY the 26 abstract RPQ patterns / 142 concrete
  queries of their `Queries.xlsx`. What was never taken: their Google
  Drive data link, their jars, their evaluation machinery. These files
  are OUR Cypher QPP translations; each file's header names the upstream
  repo and pin.
- Translation (2026-11-01 spike): the PathDB SNB edge label maps onto
  the graph-benchmark-ldbc store's relationship types — one graph-benchmark-ldbc type per SNB label where
  the store split it:

  | PathDB label | graph-benchmark-ldbc relationship types |
  | --- | --- |
  | `hasCreator` | `commentHasCreator` \| `postHasCreator` |
  | `hasModerator` | `hasModerator` |
  | `replyOf` | `replyOfComment` \| `replyOfPost` |
  | `studyAt` | `studyAt` |
  | `hasInterest` | `hasInterest` |
  | `likes` | `likeComment` \| `likePost` |
  | `isSubclassOf` | `isSubclassOf` |
  | `isLocatedIn` | `commentIsLocatedIn` \| `organisationIsLocatedIn` \| `personIsLocatedIn` \| `postIsLocatedIn` |
  | `containerOf` | `containerOf` |
  | `hasMember` | `hasMember` |
  | `isPartOf` | `isPartOf` |
  | `hasTag` | `commentHasTag` \| `forumHasTag` \| `postHasTag` |
  | `workAt` | `workAt` |

  Optional elements (`A?`) translate to `*0..1`. Alternations split by
  their arms' type sets: when both arms carry the SAME type set
  (`( A? | A )`, `( A | A+ )`, `( A | A* )`, …), the alternation is
  semantically one quantified pattern — `( A? | A ) ≡ A*0..1`,
  `( A | A+ ) ≡ A+`, `( A | A* ) ≡ A*` — and is spelled that way (the
  13 same-type entries were collapsed to the single pattern on
  2026-11-01; answers verified unchanged). When the arms differ
  (`( A? | B )` with A ≠ B, not spellable as one relationship pattern),
  the translation is a UNION of the two branches, each carrying the
  statement's LIMIT — which composes additively (see the LABS-646
  engine pin: two LIMIT-100 branches can answer more than 100). The
  restrictor matrix re-spells the 13 templates in the five path modes
  (`TRAIL`, `WALK`, `SIMPLE`, `ACYCLIC`, `WALK` under
  `REPEATABLE ELEMENTS`).

## File format

- `queries/p001..p142.cypher`, `restrictors/r001..r065.cypher`: one
  statement per file, no trailing `;`, parameters as `$seed`. The header
  is provenance prose plus exactly one machine-read marker line:
  - `// status: ok <prose>` — executed; must succeed (the ok-set
    ratchet: an `ok` entry that newly times out or errors fails the
    run; the ok-set only shrinks with a tracked reason, like the TCK
    allowlist).
  - `// status: kept <prose>` — parse and EXPLAIN only, never executed:
    the cyclic-recursion classes (`A+`/`A*`/`(A|B)+`/… over
    `knows`/`likes`) that time out today — frontier-NFA acceptance
    cases (LABS-644), and the memory blow-up / no-walk-budget cases
    (LABS-660). The mode-prefix cliff itself is closed (ADR-0471); the
    unbounded-recursion class is what keeps these entries out.
  - `// status: error-pin <ErrorName> <prose>` — executed; must fail
    with the authored `ErrorName` (the fail-closed
    `UnsafeRepeatableElements` contract for unbounded REPEATABLE
    ELEMENTS).
- `seed-keys.json`: the one canonical seed-key fact table (see below) —
  every key, both scale factors, consulted at every scale factor. The
  key is canonical — the label plus the type set sorted and
  de-duplicated — so one (label, type set) fact has exactly one
  spelling; the loader refuses any other.
- `params-sf0.1.json` / `params-sf1.json`: the derived per-query seed
  entries at that scale factor (see below). The SF1 file holds the
  spike's 26-query subset; a query without an entry at a scale factor
  is planned, not run, there. All three files are machine-recorded —
  uniform records, regenerated whole by `--seed-policy median`, never
  hand-edited — and the key table is split out precisely so the fact
  (36 keys) exists exactly once and every file stays under the 1k-line
  split rule.

## Seeds (the degree-median policy)

PathDB's own seed ids are LDBC datagen outputs and exist in none of our
stores (0 of 26 in graph-benchmark-ldbc SF1 — different datagen run), so every seed is
substituted with a node of OUR store, chosen deterministically: among
nodes of the seed label with at least one incident edge of the first-hop
types, order by `(degree, ID)` and take the lower-median element; when no
node of that label carries those types, the same policy over all labels
(one template: p095 seeds a `Comment` for `hasInterest`, which no LDBC
output carries — its key records the label the policy resolved, `Tag`,
so the query's faithful `Comment` anchor never matches: 0 rows, as in
PathDB's own data). One seed per `(seed label, first-hop types)` key —
36 keys across the workload, recorded once in `seed-keys.json`; each
record carries the chosen node's id, degree and label, and the runner
checks on load that each query's `seed` equals its key's recorded id,
that each record's `label` equals its key's label (a key never names a
node of another label — a crossing fails loudly, the
re-keying above is how p095's is made explicit). Re-freezing the data
set regenerates every seed: run the corpus with `--seed-policy median`
(the default) and it re-resolves all keys and reports the diffs — an
expected outcome, never a failure.

## Execution policy

Run-and-report (ADR-0289 memory-shape precedent): nothing here enters a
frozen gate compare set and no latency number gates a PR. What gates is
the ratchet (statuses above) and the pins. Timeout: 10 s per entry at
SF0.1 (20 s at SF1), watchdog-cancelled. Row counts are reported per
entry — the engine-self oracle; the Neo4j-verified oracle answers are
follow-up work once the faithful spelling is runnable end to end.

Initial ok-set, recorded 2026-11-01 against the engine at the corpus PR
(`query-latency corpus pathdb --sf 0.1`, graph-benchmark-ldbc SF0.1, release):
134 `ok` of 134 ok, 66 `kept` planned, 7 `error-pin` failed as pinned,
0 seed diffs, peak RSS 1.28 GB, slowest entry 2.4 s, median 0.3 ms. Ten
restrictor entries were re-measured the same day and reclassified
`kept` → `ok`: r021-r025 and r056 (4-39 ms each — bounded two-/three-hop
shapes the copied unbounded-recursion reason could not apply to), and
r006/r007/r009/r010 (~86 ms, 20 rows each — the replyOf forest is
acyclic, so the unbounded quantifier under the mode prefix terminates;
the copied cyclic-recursion reason named a mechanism this data cannot
have). The 13 same-type alternation entries (p095-p098, p113, p115,
p116, p121, p122, p124, p127, p128, p130) were collapsed to their single
quantified pattern the same day, with every ok entry's row count
verified unchanged.

## How to run

```bash
cargo run --release -p relate-query-latency -- corpus pathdb --sf 0.1
cargo run --release -p relate-query-latency -- corpus pathdb --sf 1 --timeout 20
# the gated test (loads the graph-benchmark-ldbc SF0.1 graph):
RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p relate-query-latency --test corpus pathdb
```

Full flag table: `docs/datasets.md` (the `query-latency corpus`
section).
