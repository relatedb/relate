# CI lane map and OS dedup

Implementation notes for ADR-0251 §2–§4, stacked on PR #293 (the full-Linux
plus OS-smoke/OS-filesystem/OS-surface split);
[`cadences.md`](cadences.md) covers §6, [`build-caches.md`](build-caches.md)
§5, and [ADR-0331](../adr/0331-sccache-over-mtime-normalization.md) the post-checkout
sccache setup that makes the §5 shared rust-cache keys restorable. The enforcement
point is `scripts/check-os-matrix.py` (runs in the Lint job); this document
records the decisions behind the lane shapes.

## Lane map after the dedup

| Job | Runner | Content |
| --- | --- | --- |
| `failfast` | ubuntu | Previous run's failed checks, annotated and cheap-replayed first (below) |
| `lint` | ubuntu | fmt, clippy (workspace + CLI bundles + no-parquet), boundaries, keyword probe, OS-matrix check |
| `test` | linux-4-core-x86 | Full unit/integration suite in 3 exec-balanced matrix shards (`engine`, `storage`, `planner`; LABS-310 round 3), documented Rust API examples on the engine shard (runner pilot, below) |
| `feature-bundles` | ubuntu | CLI `agent`/`minimal` bundles, engine without `parquet` |
| `stack-release` | linux-4-core-x86 | Stack-depth engine tests, release profile (ADR-0259); on the 4-core runner group with the other release-profile lanes (runner pilot, [runtime.md](runtime.md)) |
| `corpus` | linux-4-core-x86 | Corpus-profile build once, then TCK + CIP-eval + cookbook-native + the LDBC SF0.1 answer gate as steps of the same job (ADR-0252; profile split per ADR-0212; LDBC slice per ADR-0251 §6, [cadences.md](cadences.md)) |
| `python` | linux-4-core-x86 | Rust-side binding tests, wheel build, full pytest |
| `os-smoke` | macos | Storage filesystem depth + full surface smoke set (incl. Python wheel link, npm, Maven) |
| `swift` | macos | `make test-swift`: three-slice static XCFramework, `swift test`, the `docs/api/swift` examples, the release-stamp harness, the iOS example's core tests and simulator build (ADR-0433) |
| `os-filesystem` | windows | Storage filesystem depth, both feature sets, on an ImDisk ramdisk |
| `os-surface` | windows | Trimmed surface lane (below), on an ImDisk ramdisk |
| `relate-bolt` | ubuntu | Bolt suite sharded by test binary (`concurrency`/`e2e`/`smoke` legs) with the 151-test lib suite further partitioned at test level across 4 runners (`nextest --partition count:N/4`) — the concurrency battery stays a single isolated leg (timing-sensitive), plus the official-driver smoke + documented examples (the docker image smoke is the nightly `docker-smoke` lane, [cadences.md](cadences.md)) |
| `relate-clients` | ubuntu | JS/Java/Go client contract suites against the C ABI |
| `wasm` | ubuntu | wasm32 checks only — the browser smokes, release bundle, and API examples moved to the 6-hourly `wasm-full` lane ([cadences.md](cadences.md), ADR-0251 §6) |
| `npm-smoke` | ubuntu | npm pack/install/one-query smokes (`scripts/npm-smoke-test.sh`) |
| `doc-examples`, `notebook` | linux-4-core-x86 | The release-profile docs lanes (documented function examples through the release CLI; getting-started notebook against a dev-profile wheel); moved to the 4-core runner group with `stack-release` (runner pilot, [runtime.md](runtime.md)) |
| `loadtest-smoke`, `loadtest-baselines`, docs lanes | ubuntu | Unchanged (the remaining docs lanes: link-check, cookbook-wasm) |
| `memory-controls` | `vars.LOADTEST_RUNNER` | Memory control verifications and one calibration repeat (`MEMORY_CONTROLS_PROFILE=pr`). An envelope miss warns and the job stays green (ADR-0544). A behavioural miss still fails. Posts one self-updating pull-request comment. The three-repeat matrix is the nightly `memory-controls-full` cadence |
| `memory-shape` | `vars.LOADTEST_RUNNER` | Fixed graph shapes, then the min/max envelope. An envelope miss warns and the job stays green (ADR-0544). A wrong count still fails. Posts one self-updating pull-request comment. The nightly cadence runs the same stage and does not comment |
| `jepsen-harness-sanity`, `jepsen` | ubuntu | Jepsen crash-durability lane (LABS-650): the dockerized n=1 set + kill -9 harness from `testing/jepsen` against the stock product image, built on the runner with the docker-smoke recipe (buildx `--load`). Plain docker jobs — no Sonic stage, no `sonic-init` need (the `os-smoke` precedent). `jepsen` is full-CI-gated (ready-for-review) and bounded by `timeout-minutes`; every nonzero jepsen verdict (1 check failed, 2 unknown) fails it, and the run log + jepsen store upload as forensics. Drafts run only `jepsen-harness-sanity`: the compose-config validation |
| `test-8core-ab`, `python-8core-ab` | linux-8-core-x86 | Dispatch-only A/B replicas of the Test shards and the Python lane (LABS-310 round 4, [runtime.md](runtime.md)); never on the PR path. The Rust preamble itself is the shared `.github/actions/setup-rust-ci` composite action (round 4), one canonical copy for all 18 Rust lanes |

The loadtest-baselines gate note: its `make compare-loadtests` step stays
commented out (LABS-305, pre-existing) — the freeze-gate re-enable is
tracked separately, not part of the LABS-310 CI-speed rounds.

## Windows `os-surface` shrink

The lane previously built the Python extension module plus five surface
crates twice (once plain, once as test harnesses) to run five one-test
smokes and one FFI compile check. What is genuinely OS-dependent stays:

- the MSVC compile/link of the FFI cdylib (`cargo build -p relate-ffi`)
  and of the four surface test harnesses (`cargo test --no-run …`), which
  double as the surface crates' compile check — the redundant plain
  `cargo build` of the same crates is gone;
- the four one-query surface smokes (MCP stdio, Bolt, HTTP, CLI bundles);
- the cl-compiled C example (`scripts/ffi-compile-example.sh`) and the cgo
  smoke with its mingw import-library dance (`gendef`/`dlltool`), which is
  Windows-only coverage.

Dropped as OS-independent (covered elsewhere): the Python wheel build,
install, and pytest (macOS `os-smoke` and the ubuntu `python` job), and the
npm/Maven client smokes (macOS `os-smoke` and ubuntu `relate-clients`).

The macOS Python coverage was already minimal after #293: the wheel
build/link plus the single native smoke. The full pytest suite is
OS-independent and runs on ubuntu only.

## Fail-fast ordering (ADR-0251 §4)

`scripts/prev-failed-jobs.sh` reads the most recent completed CI run on the
current branch through the GH API and lists its failed jobs. The `failfast`
job (first code job in the file, `needs: changes` only) annotates those
names, writes them to the step summary, and when `Lint` is among them
re-runs `cargo fmt --all --check` immediately — a repeat formatting failure
then fails in about a minute. The API read never fails a run; a fresh
branch or a renamed check degrades to empty output (per ADR-0251
Consequences, check names are stable identifiers — a rename must update the
`Lint` match in the `failfast` job).

No `needs:` chains were added: every other job still starts in parallel.
The `corpus-build` → consumers chain is gone too — ADR-0252 merged the
four corpus jobs into one `corpus` job (one compile, four gate steps:
TCK, CIP-eval, cookbook-native, and the LDBC SF0.1 answer gate), so
the write-once-per-key ordering the cache used to force no longer exists.
Job definition order puts the cheap, high-signal gates (`failfast`, `lint`)
first so they win the runner queue under concurrency caps.

## Runner pilot (LABS-310)

The free tier stays the default for the broad parallel set; bigger runners
serve the slowest compile-heavy builds. Two lanes are piloted: `corpus`
(the compile-dominated critical path: one corpus-profile compile feeding
the TCK, CIP-eval, cookbook-native, and LDBC SF0.1 gates) and `test` (the
19-minute critical path: the full unit/integration suite plus the
documented Rust API examples), both moved from `ubuntu-latest` to
`linux-4-core-x86`. Nothing else moves. The `corpus` pilot also
drops the `Free runner disk` step on this lane: the 4-core runner ships
~150 GB of disk, and the experiment checks whether the corpus build fits
without paying the cleanup time. Before/after walls, the disk-headroom
check, and the 2x success bar are recorded in
[runtime.md](runtime.md). The loadtest/baseline lanes stay pinned to their
ADR-0070 environment class and are untouched; the Windows lanes keep ImDisk.

## Windows ramdisk (ImDisk)

Both Windows lanes mount an 8 GB ImDisk ramdisk (`choco install
imdisk-toolkit`, `imdisk -a -s 8G -m R: -p "/fs:ntfs /q"`) before the
toolchain and cache steps, and point `CARGO_TARGET_DIR` plus `TMP`/`TEMP`
at it — NTFS metadata I/O is the dominant Windows cost, and the runner has
16 GB of RAM. `CARGO_PROFILE_DEV_DEBUG=line-tables-only` keeps the debug
target inside the ramdisk (backtraces stay symbolized). The step is one
shared script, `scripts/ramdisk-imdisk.sh`, called by both Windows lanes
and by the `os-sweep` cadence lane in `background-cadences.yml` (YAML
anchors cannot cross files, which is why it is a script, not an anchor).

Fallback: any driver-install or mount failure downgrades to the plain disk
paths with a `::warning::`; the step summary records `ramdisk mode:
imdisk|disk` either way, so a flaky ImDisk shows up in the log, not as a
red run. If the ramdisk overflows (build ENOSPC on `R:`), the fix is the
same fallback — the lane still passes on disk, slower.

Linux/macOS ramdisks were weighed and rejected: `/dev/shm` on hosted ubuntu
runners is ~3.5 GB against a ~29 GB debug target, and an `hdiutil` volume
on macOS buys little over APFS for the smoke-sized lanes. Revisit only if
lane timings say otherwise.

## Smoke-lane audit (ADR-0251 §3)

All three smoke lanes are already true minimal e2e — no cuts needed:

- **npm pack/install** (`scripts/npm-smoke-test.sh`): pack, install into a
  fresh project, one `RETURN 1` query per artifact shape, exit. No
  fixtures, no second query.
- **Bolt + official driver**: one Bolt server per mode
  (plaintext/TLS/auth) through the crate's `serve(&Args)` entry point
  (ADR-0368 — the standalone binary is retired), the official Python
  driver smoke, and the
  documented docs/api/bolt examples — the examples are AGENTS.md §2 tests,
  not smoke-lane bloat.
- **wasm**: `wasm-pack test --headless --firefox` runs
  `surfaces/relate-wasm/tests/smoke.rs` — a handful of browser-API
  tests (worker, blob, read-only) that exist only on wasm32, then the
  bundle build and the documented API examples. No duplicated unit
  coverage.
