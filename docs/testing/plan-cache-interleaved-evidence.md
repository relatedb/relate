# Plan-cache admission evidence: an interleaved workload, and the p50 a hit pays

The evidence behind
[ADR-0687](../adr/0687-plan-cache-token-footprint-invalidation.md): a hot
template's plan survives the commits its token footprint does not touch, so a
read after an unrelated commit is a cache hit, not a re-plan.

Two artifacts carry it:

- `relate/relate/tests/integration/interleaved_replan_evidence.rs` — the workload and the
  per-statement outcome assertions.
- `relate/relate/tests/integration/interleaved_replan_measure.rs` — the `#[ignore]` p50
  harness whose numbers are below.

## The workload and its outcomes

`interleaved_replan_evidence.rs` drives a write+read workload and asserts the
`planCache` outcome of every statement through the public result header
(`QueryResult::plan_cache`) — the same fact every surface renders:

- **`an_interleaved_workload_hits_until_drift_or_schema_change`** seeds three
  `:User` rows and reads the point template
  `MATCH (n:User {id: <literal>}) RETURN n.name AS name`:
  - first read `Miss`; same template at another literal `HitTemplate`;
  - three interleaved `:Tag` writes, each followed by a read → `HitTemplate`
    every time (no `:Tag` token is in the `:User` footprint);
  - `3 -> 4` Users (`4/3` drift) → `HitTemplate` (re-stamped clean);
  - `3 -> 6` Users (exactly `2x`) → `HitTemplate` (marked dirty, still
    served: a point read answers one row at any label size, so its grade
    never degrades);
  - `CREATE INDEX` → the next read `Replan` (the epoch moved), then
    `HitTemplate` again, unrelated commits included.
- **`a_scan_that_outgrows_its_grade_replans_after_footprint_drift`** uses a
  whole-label scan, whose store reads grow with the label: `3 -> 90` Users
  (30x) marks the plan dirty and the drift hit is graded with the larger
  work, so the next hit is a `Replan`.

Run: `cargo test -p relate --test interleaved_replan_evidence` (2 tests
green).

## The p50 measurement

`interleaved_replan_measure.rs` seeds 10 `:User` rows and warms the
point-read template `MATCH (n:User {id: <literal>}) RETURN n.id AS id`. Each
of 1000 timed reads is preceded by one unrelated `CREATE (:Tag {t: i})`
commit; **only the read is inside the clock** — the commit is outside every
sample. The literal cycles so each read is a stage-2 template hit, not the
raw-string fast path of an identical text. The harness reports three rounds
per invocation; the runner invoked each build's binary three times, alternating
base and feature so a drifting load hit both alike.

The harness is free of this branch's admission types (it reports the outcome
via the public `PlanCacheOutcome::as_str`), so the same file compiles and runs
on the pre-change base.

### Environment

- **Machine:** Apple M4 Max, 16 logical cores, arm64, macOS 27.2.1
  (Darwin 27.2.0).
- **Toolchain / profile:** rustc 1.99.1, `corpus` profile (ADR-0212: opt-level
  2, no LTO, within ~1-5% of release).
- **Builds:** base `c9c2ea54` and feature `44682409`, each in its own
  worktree with its own `CARGO_TARGET_DIR`, both built with the same profile.
  `44682409` is the feature tip; the evidence commit adds only the two test
  files and this report, so the engine code measured is `44682409`.
- **Load — the box was not quiet.** Unrelated work ran throughout; the load
  averages (1/5/15 min) across the window were `70.06 / 53.76 / 45.62` to
  `68.77 / 53.76 / 45.67` on 16 cores. The base/feature runs were interleaved
  under that load, so the A/B is comparable, but the absolute latencies are
  inflated by contention. The categorical outcome split and the PLAN-phase
  time are the load-robust signals.
- **Run count:** 3 invocations × 3 rounds × 1000 timed reads = **9000 timed
  reads per build**.

### Command

Feature worktree:

```sh
CARGO_TARGET_DIR=$PWD/target cargo test --profile corpus -p relate \
  --test interleaved_replan_measure -- --ignored --nocapture
```

Base: copy the same harness into a `c9c2ea54` checkout, then the identical
command there:

```sh
cp <feature>/relate/relate/tests/integration/interleaved_replan_measure.rs \
   <base>/relate/relate/tests/
cd <base> && CARGO_TARGET_DIR=$PWD/target cargo test --profile corpus -p relate \
  --test interleaved_replan_measure -- --ignored --nocapture
```

### Numbers

p50 per round; PLAN-phase p50 is the engine's own `QueryResult::plan_ms`.

| build | round | wall p50 (ns) | PLAN p50 (µs) | outcomes (1000 reads) |
| --- | ---: | ---: | ---: | --- |
| base `c9c2ea54` | 1 | 45333 | 11.542 | `replan-generation-mismatch` ×1000 |
| base `c9c2ea54` | 2 | 43125 | 11.000 | `replan-generation-mismatch` ×1000 |
| base `c9c2ea54` | 3 | 43083 | 11.167 | `replan-generation-mismatch` ×1000 |
| base `c9c2ea54` | 4 | 49166 | 12.458 | `replan-generation-mismatch` ×1000 |
| base `c9c2ea54` | 5 | 46417 | 11.792 | `replan-generation-mismatch` ×1000 |
| base `c9c2ea54` | 6 | 44125 | 11.333 | `replan-generation-mismatch` ×1000 |
| base `c9c2ea54` | 7 | 48459 | 12.167 | `replan-generation-mismatch` ×1000 |
| base `c9c2ea54` | 8 | 45250 | 11.292 | `replan-generation-mismatch` ×1000 |
| base `c9c2ea54` | 9 | 49083 | 12.666 | `replan-generation-mismatch` ×1000 |
| feature `44682409` | 1 | 35209 | 2.959 | `hit-template` ×1000 |
| feature `44682409` | 2 | 37917 | 3.250 | `hit-template` ×1000 |
| feature `44682409` | 3 | 34833 | 3.041 | `hit-template` ×1000 |
| feature `44682409` | 4 | 40583 | 3.500 | `hit-template` ×1000 |
| feature `44682409` | 5 | 37208 | 3.209 | `hit-template` ×1000 |
| feature `44682409` | 6 | 36958 | 3.291 | `hit-template` ×1000 |
| feature `44682409` | 7 | 37416 | 3.167 | `hit-template` ×1000 |
| feature `44682409` | 8 | 36000 | 2.958 | `hit-template` ×1000 |
| feature `44682409` | 9 | 35958 | 3.125 | `hit-template` ×1000 |

Summaries (median over the 9 rounds):

| build | wall p50 median (min–max) | PLAN p50 median | outcome split |
| --- | --- | --- | --- |
| base `c9c2ea54` | 45.3 µs (43.1–49.2) | 11.54 µs | replan 9000 / 9000 |
| feature `44682409` | 37.0 µs (34.8–40.6) | 3.17 µs | hit 9000 / 9000 |

### What the numbers show

- **The hit path pays no re-plan.** After every unrelated commit the base
  re-plans the read (9000/9000 `replan-generation-mismatch`), while the
  feature serves it from the cache (9000/9000 `hit-template`). The PLAN phase
  a hit pays (3.17 µs median) is the footprint/epoch/grade admission check and
  the gates — not a rebuild; the base's ~11.5 µs is the rebuild.
- **The measured effect is ~3.6x on the PLAN phase and ~1.23x on the whole
  read** (45.3 µs -> 37.0 µs p50) in this workload under this load. A point
  read's execution and result handling dominate the remainder, so the PLAN
  ratio is the honest size of the admission change; the wall ratio is what a
  caller sees.
- The two drift arms and the DDL arm are pinned by the outcome test, so the
  timing above is not doing double duty as a correctness claim.

## Reproducing

1. Build the feature measurement harness:
   `cargo test --profile corpus -p relate --test interleaved_replan_measure --no-run`.
2. Repeat step 1 in a `c9c2ea54` worktree after copying the harness file in.
3. Run each binary three times, alternating base and feature, and keep the
   `PCI ` lines. Stamp the load with `sysctl -n vm.loadavg` before and after
   each invocation; a quiet box is preferred, and the load stamp is what makes
   a busy one honest.
