# Runtime verification

> **Historical record (2026-11-05).** The `relate-tck` and `relate-cip-eval`
> lanes and timing numbers below predate the corpora move (ADR-0520); the
> language corpora and their measurement live in the spec harness (ADR-0482).

Verification record for the fast-test-architecture branch (LABS-310) and the
seed of the ADR-0251 §7 runtime dashboard: the per-suite before/after table,
the continuous lanes that still exceed the ≤ 60 s ambition, and the coverage
ledger proving nothing was lost on the way. [`cadences.md`](cadences.md)
owns the lane → cadence assignment and [`ci-lanes.md`](ci-lanes.md) the lane
shapes; this page owns the numbers.

## Method

After-numbers measured on the final branch (2026-10-15, Apple-silicon
laptop): `cargo test -p <crate>` per suite, run serially so wall times are
honest, warm `target/` from the measurement order (the first suite carries
the engine compile). Before-numbers are the tier audit's baseline sweep on
the same machine class, recorded in the LABS-310 store. Wall time includes
whatever incremental compile the suite needed; that is the number a
developer feels.

## Per-suite before / after

| Suite (`cargo test -p …`) | Before (audit, s) | After (branch, s) | Note |
| --- | --- | --- | --- |
| `relate` | 206.7 (117.9 exec) | 141 | First in the sweep — carries the engine compile |
| `relate-storage` | 61.1 | 48 | Disk tier by design ([test-tiers.md](test-tiers.md)) |
| `relate-executor` | 51.2 | 37 | |
| `relate-cli` (default) | 58.2 | 48 | e2e lanes spawn the binary; stays disk |
| `relate-bolt` | — (not in audit sweep) | 16 | Concurrency suite re-tiered to `:memory:` |
| `relate-load` | — (not in audit sweep) | 4 | Streaming suite re-tiered to `:memory:` |
| `relate-datasets` | 66.6 | 67 | Fixture generation dominates; unchanged |
| `relate-http` | — (not in audit sweep) | 3 | |
| `relate-mcp` | 26.5 | 7 | |

Audit-baseline suites not re-measured locally because they are CI-shaped,
not `cargo test -p` shaped: `go-client` 43.7 s (client contract lane),
`tck-corpus` 38.7 s wall / 2.5 s exec (the corpus-profile TCK — re-verified
green, 3888/3888, exec still seconds), `cookbook` 25.4 s and
`query-latency` 24.2 s (steps of the merged `corpus` job, ADR-0252).

The deltas are modest by design: the re-tier moved six tests and the cache
work pays in CI, not on a warm laptop. The honest statement is that nothing
regressed while the CI machine-time work (failfast, OS-surface shrink,
shared build caches, corpus-job merge, cadences) landed.

## Compilation profile per suite (ADR-0253)

Measured 2026-10-15 on the same Apple-silicon laptop, suites run serially in
the listed order against a warm shared target dir (the first suite in each
profile carries the shared engine compile for that profile). Compile is
`cargo test [--profile P] --no-run -p <suite>`; exec is the immediately
following `cargo test -p <suite>`. `test-optN` is the dev-family test
profile with `opt-level = N` (debug assertions and overflow checks stay
**on**, unlike corpus/release).

| Suite | Profile | Compile (s) | Exec (s) | Total (s) |
| --- | --- | --- | --- | --- |
| `relate` | dev | 25 | 200 | 225 |
| `relate` | test-opt1 | 41 | 14 | 55 |
| `relate` | test-opt2 | 53 | 18 | 71 |
| `relate` | corpus | 41 | 13* | 54 |
| `relate` | release | 56 | 17* | 73 |
| `relate-executor` | dev | 12 | 40 | 52 |
| `relate-executor` | test-opt1 | 25 | 7 | 32 |
| `relate-executor` | test-opt2 | 28 | 10 | 38 |
| `relate-executor` | corpus | 22 | 7 | 29 |
| `relate-executor` | release | 22 | 8 | 30 |
| `relate-storage` | dev | 10 | 49 | 59 |
| `relate-storage` | test-opt1 | 25 | 37 | 62 |
| `relate-storage` | test-opt2 | 32 | 45 | 77 |
| `relate-storage` | corpus | 23 | 36 | 59 |
| `relate-storage` | release | 24 | 38 | 62 |
| `relate-datasets` | dev | 17 | 63 | 80 |
| `relate-datasets` | test-opt1 | 33 | 27 | 60 |
| `relate-datasets` | test-opt2 | 46 | 22 | 68 |
| `relate-datasets` | corpus | 32 | 19 | 51 |
| `relate-datasets` | release | 41 | 31 | 72 |
| `relate-model` | dev | 5 | 3 | 8 |
| `relate-model` | test-opt1 | 3 | 3 | 6 |
| `relate-model` | corpus | 6 | 3 | 9 |
| `relate-front` | dev | 3 | 1 | 4 |
| `relate-front` | test-opt1 | 7 | 2 | 9 |
| `relate-front` | corpus | 7 | 1 | 8 |

\* The corpus/release `relate` runs failed three cancellation tests
whose workloads were calibrated to dev-profile speed (un-cancelled control
finished in 49-78 ms, under the 80 ms floor); the tests now escalate their
workload until the control provably outlasts the cancel latency and pass at
every profile. Exec shown is the aborted run's wall and is representative:
the same suites at test-opt1 (where the failure was identical pre-fix) run
to completion in the same time.

**Choices, from the matrix:**

- The four exec-heavy suites (`relate`, `relate-executor`,
  `relate-storage`, `relate-datasets`) get **test-opt1**: exec drops
  15x / 5.7x / 1.3x / 2.3x. Storage's total is a wash on a warm laptop
  (compile +15 s vs exec −12 s); it still gets opt1 because exec dominates
  every re-run and the CI compile is cache-amortized (below).
- The cheap controls (`relate-model`, `relate-front`, and the other
  parser/planner/catalog suites) exec in ≤ 3 s at any profile; they ride the
  same workspace-wide `[profile.test] opt-level = 1` (the knob cannot be
  scoped per invocation — CI builds all core test targets in one `cargo
  test` call) at ≤ 4 s extra warm compile each. No dedicated profile is
  justified.
- **test-opt2 is rejected:** exec is statistically identical to opt1
  (storage and the `relate` control run *slower*) for 25-45% more
  compile.
- **corpus/release for unit suites is rejected:** the corpus profile
  disables `debug_assert!` and overflow checks, and the `test`-job unit
  suites are exactly where those contracts are evaluated (see the ci.yml
  `test` job comment). `release` is additionally dominated by `corpus` on
  every row. The corpus family (TCK, CIP-eval, cookbook, query latency)
  keeps the corpus profile unchanged (ADR-0212, ADR-0252).

**CI deltas:** the `test`, `feature-bundles`, OS-delta, and other
dev-shared-key jobs now compile test targets at opt-level 1 (the
`CARGO_PROFILE_DEV_DEBUG=line-tables-only` trim and `CARGO_INCREMENTAL=0`
are unchanged). The extra codegen cost lands on the shared `dev` rust-cache
build — paid on Cargo.lock changes, not per PR — while the exec win
(`relate` alone: −186 s of the ~2.5 min lane wall called out in the
lane table above) is paid back on every run. The corpus job is unchanged.
No merge-gate suite regresses locally: every row above at the chosen
profile is within noise of or far below its dev total, except
`relate-storage` (+3 s warm, wash) and `relate-front` (+5 s warm).

## Continuous lanes over the ≤ 60 s ambition

ADR-0251 Consequences sets ≤ 60 s per continuous suite as the design
constraint. Met by construction: `changes` (path detection) and `failfast`
(annotation + at most a `cargo fmt` replay). Every other continuous lane in
`.github/workflows/ci.yml` still exceeds 60 s; each is named here with the
reason, which is almost always **compile, not test execution** — the shared
build caches ([build-caches.md](build-caches.md)) attack exactly that term.

| Lane | Why it exceeds 60 s |
| --- | --- |
| `lint` | Workspace clippy (all targets, three CLI feature combos) recompiles the tree under the clippy driver |
| `test` | Full unit/integration suite; the 604-test `relate` crate alone is ~2.5 min wall locally |
| `feature-bundles` | Two more CLI compile configurations (`agent`, `minimal`) plus no-parquet engine |
| `stack-release` | Release-profile compile for the stack-depth tests (ADR-0259) |
| `corpus` | One corpus-profile (optimized) compile feeding TCK + CIP-eval + the LDBC SF0.1 answer gate (ADR-0252); execution is seconds, the compile is not |
| `python` | pyo3 extension build, wheel packaging, full pytest |
| `os-smoke` (macOS) | Storage filesystem depth + the full surface smoke set, incl. wheel link, npm, Maven |
| `swift` (macOS) | Five release-profile `relate-ffi` cross-compiles for the XCFramework, then the Swift package, examples and the iOS app build |
| `os-filesystem`, `os-surface` (Windows) | MSVC compile/link dominates even on the ImDisk ramdisk ([ci-lanes.md](ci-lanes.md)) |
| `relate-bolt` | Official-driver smoke (the docker image smoke moved to the nightly `docker-smoke` lane, [cadences.md](cadences.md)) |
| `relate-clients` | JS/Java/Go toolchains against the C ABI |
| `wasm` | Two wasm32 `cargo check`s — nearest the target; compile-dominated |
| `npm-smoke` | npm pack/install/one-query |
| `loadtest-smoke` | Dev-profile dataset suites at `ci()` scale |
| `loadtest-baselines` | The pinned ADR-0070 Docker environment; intentionally not host-fast |
| docs lanes (`doc-examples`, `notebook`) | Example builds |

This table is the dashboard's seed: a lane leaves it when a cache or split
change brings it under 60 s, and §7's dashboard tracks the shrinking
residue rather than re-deriving it.

## Runner pilot: `corpus` and `test` on linux-4-core-x86 (LABS-310)

Michael's runner policy: the free tier stays for the broad parallel set;
the bigger runners serve the slowest compile-heavy builds. The `corpus`
job is the compile-dominated critical path (one corpus-profile compile
feeding the TCK, CIP-eval, cookbook-native, and LDBC SF0.1 gates), so it
was the first piloted lane: `runs-on: ubuntu-latest` ->
`linux-4-core-x86`. The `test` job — the 19-minute critical path, the
full unit/integration suite plus the documented Rust API examples —
joined the pilot on the same runner group; its steps, cache keys, and
mtime normalization are unchanged. The release-profile lanes that build
binaries and need disk headroom followed (round 4 follow-up):
`stack-release`, `doc-examples`, and `notebook` moved from `ubuntu-latest`
to the same 4-core group — the 150 GB-SSD runner skips the free-disk clean
via the `MIN_FREE_MB` early-exit, halving the cold release build. Nothing
else moves; the loadtest/baseline lanes stay pinned to their ADR-0070
environment class (the OS lanes are untouched).

**Before wall (free runner).** The merged `corpus` job (ADR-0252) has no
CI history yet — main still runs the pre-merge chain. The closest main
data points, from the latest green main ci.yml run (2026-10-14, run
38651889782, all on `ubuntu-latest`):

| Main job | Wall |
| --- | --- |
| Corpus build (the compile this job absorbs) | 3m49s |
| TCK (openCypher) | 3m36s |
| CIP evaluation suite | 4m05s |
| Cookbook recipes (native) | 5m56s |

Serialized on one runner that is ~9m45s of critical path; merged into one
job with a single compile and second-scale step execution, the expected
free-runner before-wall is roughly 6-8 min, dominated by the
corpus-profile compile. The first CI run of this branch records the true
before/after pair (the free-tier before can be re-measured by reverting
the one-line `runs-on` change if needed).

**Pilot row** (after-wall filled in on the first CI run of this branch):

| Job | Runner (before) | Before wall | Runner (after) | After wall | Success bar |
| --- | --- | --- | --- | --- | --- |
| `corpus` | ubuntu-latest | ~6-8 min (estimate; main proxy above) | linux-4-core-x86 | **6m0s** (first branch CI run, 38668276293; TCK + CIP + cookbook + LDBC gates all green, no disk-cleanup step run) | 2x wall drop vs. the true free-runner before — the true merged-on-free-runner before was never measured (the merge landed with the pilot); re-run on ubuntu-latest by reverting the one `runs-on` line if the comparison is needed |
| `test` | ubuntu-latest | **19m06s** (branch CI run 38695797829, Test (ubuntu-latest)) | linux-4-core-x86 | to be filled on the first CI run after the move | material wall drop vs. 19m06s; re-run on ubuntu-latest by reverting the matrix `os` entry if the comparison is needed |

**Cleanup dropped on the piloted lane (Michael's experiment).** The
`Free runner disk` step (`scripts/free-disk.sh`) is omitted from the
`corpus` job entirely. Rationale: the free-tier ubuntu runners ran out of
disk in the linker (~14 GB free), which is why the step exists there; the
4-core runner ships ~150 GB of disk, so the hypothesis is that free disk
suffices and the cleanup time is saved outright. After the first run,
record the post-build disk headroom (`df -h` on the runner, or the
largest `target/` reading in the log) here; reinstate the step if the
corpus build ever approaches the cap. Free-tier lanes keep the step
unchanged.

## CI runtime round 2: nextest, mold, cache-key hygiene (ADR-0254, ADR-0255)

The follow-up round on top of the runner pilot, landed together; CI
before/after pairs are recorded from the first green run of the branch.

### cargo-nextest pilot (ADR-0254)

`cargo test -p A -p B …` compiles all test binaries and then runs them
**one binary at a time** (verified in the run-38706915770 log: the
binaries' result lines print back-to-back). The `test` job's core step was
8m48s wall on linux-4-core-x86 with the CPU idle between binaries.
`cargo nextest run` schedules individual tests from all binaries across a
process pool; compilation is unchanged (same artifacts, same rust-cache).

Local measurement (warm compile, Apple Silicon laptop — more cores than the
4-core CI runner, so the CI run-phase win is at least this):

| Suite | `cargo test` (serial binaries) | `cargo nextest run` | Speedup |
| --- | --- | --- | --- |
| `relate-cli` (31 binaries, 658 tests) | 39.7 s wall | 15.8 s execution / 22.7 s wall | 1.75x |

Lanes switched: `test` (core crates), `feature-bundles`, `relate-bolt`.
Each gains a `cargo test --doc` step where the switched invocation
previously covered doctests (nextest cannot see them). Installed prebuilt
via `scripts/install-nextest.sh` (authenticated GitHub release download),
never `cargo install`.

Deliberately NOT switched:

- **TCK corpus gate** (`cargo test --profile corpus -p relate-tck`):
  one test binary, 3892 scenarios inside one process on a big-stack worker,
  with the pass/skip summary gate computed over that single run
  (ADR-0252). Per-test process scheduling breaks both the accounting and
  the worker model; nextest has nothing to parallelize in one binary.
  *(Round 4 re-examined this: the gate tests are self-contained and the
  accounting survives per-process scheduling — the TCK step switched to
  nextest; see the round-4 section.)*
- **Corpus job siblings** (cip-eval, cookbook, query-latency), the
  stack-release suite, datasets, python: single-binary or
  single-crate suites where there is no inter-binary serialization to
  remove.
- **Doctests**: invisible to nextest; they keep stock `cargo test --doc`.

Flake policy (`.config/nextest.toml`): `retries = 1` globally (reported as
FLAKY, never silently green), **except** `package(relate-bolt)` at
`retries = 0` — the bolt concurrency suite asserts on real socket
deadlines, and a transparent retry could mask a determinism regression;
stock `cargo test` gives those tests zero retries today, so `retries = 0`
is CI parity, not a tightening. One flake was observed during local gate
development (`handshake_timeout_mid_versions_starts_no_session`, a 200 ms
real-time server-side timeout) while the machine was running three heavy
compile gates concurrently with the nextest run; a re-run on an idle
machine passed 199/199 in 4.5 s. If that family ever flakes on a loaded
CI runner, the fix is a serial test-group for the real-time tests, not a
retry.

Measured nextest gate runs on this branch: `cargo nextest run -p
relate-cli` 658/658 (and 311/311 with `--no-default-features
--features minimal`), `-p relate-bolt` 199/199.

### mold on every Linux compile lane (ADR-0255)

All ci.yml lanes that compile Rust on Linux install mold via
`taiki-e/install-action` and set `RUSTFLAGS=-C link-arg=-fuse-ld=mold`
through `$GITHUB_ENV` **before** the rust-cache step (the flag feeds the
cache env hash, and a RUSTFLAGS change invalidates cargo fingerprints, so
restoring a pre-mold cache would be a wasted download). `.cargo/config.toml`
was deliberately not used: it is checked in and would rewrite local builds.
The first run after landing is a cold compile per shared-key family; CI
link-time deltas are recorded here after that run.

`CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=line-tables-only` moved
from test-job `GITHUB_ENV` steps / per-job env blocks to one workflow-level
`env:` block — see the rust-cache section of
[build-caches.md](build-caches.md) for why per-job env drift silently
fragmented the `dev` shared cache.

Diagnostic: the `test` job runs `cargo build --timings -p relate-cli
--tests` and uploads the HTML report (`cargo-timings-test` artifact) so the
serial compile tail behind the sharding question is measurable without an
instrumented run. (LABS-310 round 3: 42 s per push was no longer buying
anything once the sharding question was answered — the step and its upload
now run on `workflow_dispatch` only, artifact per shard.)

### Test-job sharding investigation (the "split it?" question)

Question (Michael): with the test job still the slowest lane on the bigger
container, does it need a 2-way matrix split?

Numbers going in (run 38706915770, linux-4-core-x86, cold-ish cache after
the #300 Cargo.lock change): total 10m47s = 33s free-disk (removed this
round) + 8m48s core test step (compile-dominated serial tail + serial
binary execution) + 1m04s examples + ~1m40s setup/teardown.

Projection of the landed changes:

- **Sharding into 2 matrix jobs** would split the test-binary compile+run,
  but each shard repays checkout/toolchain/cache-restore (~1 min) and both
  shards restore the same `dev` shared cache while only ONE can save it
  (rust-cache save is last-writer-wins per key), so one shard always
  recompiles what the other built. The dep compile (~all of the engine) is
  identical in both shards and would be duplicated work on a cold key.
- **nextest + mold in one job** removes the serial run phase (1.75x
  measured locally on the CLI suite) and attacks the link-heavy compile
  tail (mold's design point is exactly these large debug binaries), with
  zero duplicated compile.

Recommendation: **single job, no shard** — sharding pays a second compile
of the shared dep graph whenever the key is cold, which is precisely the
regime that makes the lane slow. Revisit only if the `cargo-timings-test`
artifact on a warm-cache run shows the critical path is still the
single-job compile tail.

**Round-3 resolution (2026-10-15):** the revisit condition fired — with
the dev-v2 shared key restoring genuinely warm (run 38744308547: the Test
lane had zero `Compiling` lines), compile is off the critical path and
execution is the floor, so the duplicated-compile objection no longer
applies. The Test lane is now 3 matrix shards on the same runner class,
grouped by the measured exec numbers in the matrix above: `engine`
(`relate`, ~118 s exec, alone), `storage` (`relate-storage` ~45 s
+ `relate-cli`/`relate-ffi`/`relate-load`), `planner`
(`relate-executor` ~38 s + the ≤ 3 s light suites — joined 2026-11-02
by the three first-party extension children
`relate-wire`/`relate-fs-to-graph`/`relate-git-to-graph`,
0.06 s / 0.18 s / 0.53 s exec measured on the Apple-silicon laptop, so
they ride the lighter mixed shard under the same balancing rule — the
same day's review round completed the partition:
`relate-ontology` (79 tests across unit/acceptance/depth, ~0.1 s
exec) and `relate-markdown` (79 tests, <0.1 s) were members with
no shard and no lane, and the test kit `relate-test-support`
(20 tests, ~1.5 s) had its own suite running nowhere; all three
joined, and the partition is now guarded repo-side by
`.github/scripts/tests/test-shard-coverage.bats` — a member that
slips out of every shard fails CI instead of silently losing
coverage). All three shards share the dev key with cold-miss-only
`save-if`; the examples step runs on the `engine` shard only.

### npm-smoke lane shave (LABS-312 hygiene)

Step timings, run 38706915770 (6m08s total): checkout+setup ~25 s,
free-disk 45 s, FFI build 55 s (partially warm dev cache), **wasm-bindgen-cli
install 1m59s**, binaryen 3 s, smoke script 2m03s (npm pack × 3, three
`npm install`s of the packed tarballs pulling koffi + the published
optional platform packages cold, and the `--mode dev` wasm32 bundle build
when `pkg/` is missing).

Dominant, fixable: `scripts/install-wasm-bindgen.sh` ran `cargo install
wasm-bindgen-cli --locked --force` — a from-source compile whenever the
cached `~/.cargo/bin` copy was absent or behind the Cargo.lock pin. The
script now downloads the upstream prebuilt tarball
for the pinned version, verifies its published `.sha256sum`, and falls back
to the source build on hosts without one. Measured locally: ~3 s vs the
~2 min compile. The same fix benefits `cookbook-wasm`, and the nightly
browser lane's `cargo install wasm-pack --version 0.13.1 --locked` (2-4 min)
became `scripts/install-wasm-pack.sh` at wasm-pack 0.13.1 (ADR-0255),
downloaded through the GitHub API.

Not pursued: npm caching for the smoke's temp-project installs (no lockfile
to key on; the tarball deps are two small registry fetches), and the
in-script wasm dev build (already warm via the `dev` shared-key cache on
repeat runs of an unchanged Cargo.lock).

### Annotation fixes this round

- `loadtest-baselines` emitted hundreds of rust-cache post-job
  `ENOENT opendir …/target/.cargo-docker/registry/…` errors: the
  baseline-env docker build kept its in-container CARGO_HOME under
  `target/`, and rust-cache's registry cleanup walked the root-owned tree
  with dangling `tests/target` symlinks. `scripts/baseline-env.sh` now puts
  it at the repo root (`/.cargo-docker`, git-ignored), outside every
  cached/walked path.
- Node.js 20 action deprecation: all actions in ci.yml,
  background-cadences.yml and create-loadtest-baselines.yml bumped to their
  Node-24 majors (checkout v7, setup-node v7, setup-python v7, setup-go v7,
  setup-java v5, upload-artifact v7, docker/login-action v4,
  docker/setup-buildx-action v4). Exceptions: `ilammy/msvc-dev-cmd` has no
  Node-24 release (still v1, node20 — revisited when upstream ships one);
  `dtolnay/rust-toolchain` (rev-pinned by design) and
  `lycheeverse/lychee-action@v2` are composite actions with no Node
  runtime; `Swatinem/rust-cache@v2` already targets node24.

## CI runtime round 4: 8-core A/B, composite preamble, nextest TCK, timings dashboard

### Composite setup action (`.github/actions/setup-rust-ci`)

The sccache + rust-toolchain + free-disk + mold + rust-cache
preamble was hand-copied into 18 ci.yml jobs; one copy already drifted (the
free-disk-on-4-core bug, fixed in round 3). The preamble is now one
composite action with per-lane inputs (`sccache`,
`toolchain-components`/`toolchain-targets`, `free-disk`,
`free-disk-aggressive`, `mold`, `cache`, `cache-shared-key`,
`cache-save-if-cold-only`, `cache-workspace-crates`). Behavior is
byte-identical to the replaced blocks — same steps, same order (sccache →
toolchain → free-disk → mold install → mold env → cache), same env writes;
verified by expanding every job's step list against the pre-change
workflow. The `failfast` job keeps its bespoke conditional rustfmt-only
toolchain (it installs no toolchain at all unless the previous run failed
Lint). The corpus job is the one intentional step-level change: it gains a
nextest install for the TCK swap below.

### nextest on the TCK corpus gate (ADR-0254 extension)

Round 2 left `cargo test --profile corpus -p relate-tck` on stock cargo
test on the assumption that per-test process scheduling would break the
gate's accounting. Inspection says otherwise: the gate is three
self-contained `#[test]`s in `tck_integration.rs` (clauses, expressions,
full-summary) plus the extension-corpus test; `test_tck_full_summary`
asserts the 3892 denominator, the allowlist equality, and the skip count
over its own single run, so splitting tests across processes changes
nothing about the accounting. Local pilot
(`cargo nextest run --cargo-profile corpus -p relate-tck`, 2026-10-15,
this branch): **55/55 passed in 3.2 s**, including
`test_tck_full_summary` — the same pass/skip counts as the stock run. The
corpus TCK step now runs `cargo nextest run --cargo-profile corpus -p
relate-tck`. One deliberate semantic difference: nextest's global
`retries = 1` applies, so a genuine TCK failure is reported after one
re-run (deterministic asserts re-fail; the retry only costs wall time on an
already-red lane). CIP-eval, cookbook, and query-latency stay on stock
`cargo test` — single-binary suites with nothing for nextest to schedule.

### Per-suite profile depth re-check (ADR-0253 stands)

Round-4 question: should any exec-heavy suite run a higher profile than
the workspace-wide `[profile.test] opt-level = 1`? Re-measured on this
branch (2026-10-15, same laptop, loaded machine — absolute numbers run
~1.5-3x above the quiet-machine matrix above, but each pair was measured
back-to-back so the comparison holds; every run green at both profiles,
so the cancellation-test workload fix holds at corpus speed):

| Suite | Exec at test-opt1 (s) | Exec at corpus (s) | Verdict |
| --- | --- | --- | --- |
| `relate` | 239 | 245 | wash (corpus slightly worse) |
| `relate-executor` | 21 | 22 | wash |
| `relate-storage` | 64 | 72 | opt1 better |
| `relate-datasets` | 40 | 36 | corpus 10% better — not worth a second profile |

**Decision: unchanged.** No suite moves off the workspace-wide
`[profile.test] opt-level = 1`. Exec at opt1 is within noise of (or better
than) corpus for three of the four heavy suites; the datasets delta (~4 s
on a quiet machine) is smaller than the per-invocation compile delta a
second profile would cost, and the corpus profile disables `debug_assert!`
and overflow checks — the unit suites are exactly where those contracts
are evaluated. ADR-0253 needs no amendment.

### 8-core A/B (dispatch-only, not yet blocking)

The `linux-8-core-x86` runner is still provisioning. Two
`workflow_dispatch`-only jobs replicate the continuous lanes on it:
`test-8core-ab` (the Test lane's three shards via the `&test_shards` /
`&test_steps` YAML anchors, so the A/B cannot drift from the lane it
measures) and `python-8core-ab` (`&python_steps`). Trigger with
`gh workflow run ci.yml --ref <branch>` once the runner exists; the
default PR path is untouched.

Protocol:

- **Before** (4-core / free tier, run 38744308547, 2026-10-15): Test lane
  (pre-shard, linux-4-core-x86) **6m49s** wall; Python (ubuntu-latest)
  **7m35s** wall. The sharded per-shard 4-core before-numbers come from the
  first CI run of the round-3 branch.
- **After**: fill in from the first dispatch of `test-8core-ab` /
  `python-8core-ab` here.
- Success bar: a material wall drop on the exec-bound shards (engine
  especially — nextest already parallelizes its binaries, so the marginal
  core count is the open question). Only a clear win justifies moving the
  continuous lanes; the A/B jobs stay dispatch-only either way.

### Runtime dashboard refresh

The per-lane tables on this page are refreshed manually, not by a CI job:

```bash
scripts/ci-timings-report.py              # latest completed ci.yml run on this branch
scripts/ci-timings-report.py 38744308547  # a specific run
scripts/ci-timings-report.py --no-logs    # skip the log download (no disk column)
```

The script reads job timings via `gh run view --json jobs` and greps the
run log for each lane's `scripts/report-disk.sh` disk-headroom telemetry
line, emitting a markdown table (lane, conclusion, wall, disk free at job
end) to paste over the tables above (argparse: `[RUN_ID] [--no-logs]`).

### Still parked

`loadtest-baselines`' `make compare-loadtests` step stays commented out
(LABS-305, pre-existing): the freeze-gate re-enable is tracked separately
and is deliberately not part of this CI-speed round.

## Coverage ledger

Per-suite test counts (test-attribute count over each crate's sources,
`origin/main` vs the branch head) — equal everywhere; the re-tier moved
tests between stores, it did not add or remove any:

| Suite | main | branch | Delta |
| --- | --- | --- | --- |
| `relate` | 604 | 604 | — |
| `relate-storage` | 381 | 381 | — |
| `relate-executor` | 509 | 509 | — |
| `relate-cli` | 670 | 670 | — |
| `relate-bolt` | 199 | 199 | — |
| `relate-load` | 124 | 124 | — |
| `relate-datasets` | 80 | 80 | — |
| `relate-http` | 39 | 39 | — |
| `relate-mcp` | 51 | 51 | — |

- **TCK allowlist untouched:** `git diff origin/main..HEAD --stat` does not
  list `testing/relate-tck/src/allowlist.txt`; the corpus gate passes
  3888/3888 on the branch.
- **Disk usage stays in the justified tier:** the branch's only test-file
  edits are the six re-tier moves recorded in
  [test-tiers.md](test-tiers.md) (bolt concurrency, load streaming, the
  memory-shape pins to `:memory:`; the WAL-replay and probe tests keep real
  directories for the reasons recorded there). A workspace-wide grep for
  `tempdir`/`TempDir`/`temp_dir()` finds only the pre-existing disk tier:
  `relate-storage` (its subject is the filesystem), engine persistence
  tests (WAL/checkpoint/locking), CLI e2e (spawns the binary against store
  paths), and the loadtest/fixture harnesses. No new disk usage entered the
  memory-eligible suites.

## Gates green on the branch (2026-10-15)

`cargo fmt --all --check`; `cargo clippy --workspace --all-targets
--exclude relate-python -- -D warnings`; clippy CLI `agent` and
`minimal` combos; the nine timed suites above (all `ok`);
`cargo test --profile corpus -p relate-tck` (3888/3888); `cargo xtask
check-boundaries` (34 packages, 122 edges, 0 violations); YAML parse of
`ci.yml` and `background-cadences.yml`; `scripts/check-os-matrix.py`.

Round 4: YAML parse of `ci.yml` (anchors `&test_shards`/`&test_steps` /
`&python_steps` resolve into the A/B jobs) and
`.github/actions/setup-rust-ci/action.yml`; `scripts/check-os-matrix.py`;
step-sequence expansion of every ci.yml job against the pre-change
workflow (byte-identical everywhere except the corpus job's intentional
nextest install + TCK runner swap); `cargo nextest run --cargo-profile
corpus -p relate-tck` (55/55, incl. the 3888/3888 full-summary gate);
the eight profile re-measurements above (all green, both profiles);
`scripts/ci-timings-report.py` verified against run 38744308547 with and
without the log download.
