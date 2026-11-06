# Background cadences

Implementation notes for ADR-0251 §6. Continuous CI (`.github/workflows/ci.yml`)
keeps a representative fast slice of every heavy gate; the full versions run on
timed cadences in `.github/workflows/background-cadences.yml` (plus the
pre-existing `proptest-nightly.yml` and `graph-examples-nightly.yml`). Coverage
never shrinks: every cadenced lane is a fuller form of a continuous slice, not
a replacement for it.

## The contract

**A red cadence lane files a ticket; it never blocks a PR.** The continuous
slices remain the merge gates. Ticket filing is manual or bot-driven (the
workflow itself does not open issues); a failing nightly or 6-hourly run is a
next-day fix obligation with the failing run attached, exactly as ADR-0251
Consequences records for the TCK. Scheduled triggers fire on the default
branch only; `workflow_dispatch` runs every lane on a feature branch before
merge.

## Lane → cadence map

| Lane | Continuous slice (ci.yml, blocks PRs) | Background full version | Cadence |
| --- | --- | --- | --- |
| TCK | — (retired with the in-repo harness, ADR-0520; language coverage is the spec-harness lane, ADR-0482) | — | — |
| Loadtest baselines | `loadtest-baselines` job: fraud + identity in the pinned ADR-0070 Docker env | — (stays continuous, see below) | continuous |
| Loadtest / large fixtures | `loadtest-smoke`: dataset suites at `ci()` scale (0.05), comparator + install.sh harness tests | `large-fixtures`: full-scale (scale=1.0) fraud + identity fixtures (`RELATE_FULL_DATA=1`, release profile) | nightly 03:30 UTC |
| LDBC query latency (ADR-0243/ADR-0244) | `corpus` job step: SF0.1 answer gate (`RELATE_QUERY_LATENCY=1`, answers only, no latency judgement) | `ldbc-sf01`: all 30 queries, `--min-time 5`, p50/mean/p99 report uploaded as an artifact | nightly 03:30 UTC |
| Docker image smoke (ADR-0128) | — (the smoke itself moved fully off the merge path, LABS-310; the `relate-bolt` job keeps the driver smokes, which run against the crate's in-process `serve(&Args)` listener, ADR-0368. The `jepsen` lane does build the stock image per ready PR — see below) | `docker-smoke`: full in-container `make docker` musl build + `docker_smoke.py` | nightly 03:30 UTC |
| wasm | `wasm` job: the two wasm32 `cargo check`s | `wasm-full`: headless-Firefox browser smokes, release-wasm bundle (ADR-0100), API examples | 6-hourly (:17) |
| Multi-OS sweep | `os-smoke` (macOS), `os-filesystem` + `os-surface` (Windows): the OS-dependent delta — storage filesystem depth + one-query surface smokes | `os-sweep` (macOS + Windows): storage depth plus the **complete** surface harness suites (MCP stdio, Bolt, HTTP, CLI e2e bundles) | 6-hourly (:17) |
| Proptest elevated counts | `test` job: storage property suites at the default 256 cases | `proptest-nightly.yml`: 10000 in-memory / 1024 recovery cases (ADR-0268) | nightly 03:00 UTC |
| Graph examples | (docs lanes) | `graph-examples-nightly.yml` | nightly |

Runner choice: `os-sweep` runs on the free-tier `macos-latest` /
`windows-latest` runners, not the `windows-16-core` runners the continuous
Windows lanes use. It is a non-gating cadence — a red lane files a ticket,
it never blocks a PR — so the bigger runner's wall-clock saving buys
nothing and the sweep stays on the free tier. The 8 GB ImDisk ramdisk
still fits: the free-tier Windows runner has the same 16 GB of RAM the
continuous lanes size against.

The docker image smoke (LABS-310) is the one cadenced lane whose continuous
slice is `—`: the full in-container `make docker` musl build is a
from-scratch guarantee (~18 min on a cold buildx cache, LABS-144), and a
from-scratch guarantee cannot have a fast continuous slice — a
prebuilt-binary image build would test the packaging path, not the
guarantee. The continuous `relate-bolt` job keeps everything that runs
against the crate's serving entry points (unit suite, official Python
driver smoke, documented examples, JS WebSocket smoke — the standalone
binary is retired, ADR-0368). A red nightly files a ticket like
every other cadence lane.

One later lane re-pays the image build on the merge path on purpose: the
`jepsen` job (LABS-650, full-CI-gated) builds the stock product image on
the runner with the same docker-smoke recipe, because the crash-durability
certification must SIGKILL and WAL-replay the exact artifact users get —
a cargo-built binary would certify the wrong thing. The nightly
`docker-smoke` remains the only from-scratch packaging guarantee and
image smoke; `jepsen` never runs `docker_smoke.py`.

## Reconciliations against the ADR-0251 §6 sketch

ADR-0251 §6 sketched two demotions that the implementation does **not** carry
out (recorded as a Later-references note on ADR-0251, which is immutable):

- **TCK full corpus stayed continuous — until the corpus retired.** §6 moved
  the full corpus nightly on the assumption that it is heavy. ADR-0252 then
  merged the corpora into one `corpus` job with a single corpus-profile
  compile, and the TCK run itself is seconds, so the merge gate kept the full
  3892/3892 check on every PR. The corpus job's TCK and cip-eval steps and
  the in-repo harness have since retired (ADR-0520): the corpora live in the
  spec harness (ADR-0482), which reports language coverage without blocking.
- **Loadtest baselines stay continuous.** §6 listed them as nightly, but the
  `loadtest-baselines` job is the ADR-0070/ADR-0071 CI-owned freeze gate —
  envelope comparison only means something on the merge path. Demoting it
  would let a regression land and be discovered a night later, after merge.
  ADR-0070's pinned environment and ADR-0071's freeze workflow are
  untouched.

## Runtime notes (the ≤ 60 s ambition)

ADR-0251 Consequences sets ≤ 60 s per continuous suite as a design
constraint. Where the cadence split lands each lane:

- **Met by construction:** `failfast` (annotation + at most a `cargo fmt`
  replay), `changes` (path detection).
- **Met where the job content allows:** the trimmed `wasm` job is two
  `cargo check`s against the shared cache; `loadtest-smoke` is a dev-profile
  test run at 5% fixture scale.
- **Cannot meet it, and why:** the `corpus` job pays one optimized
  (corpus-profile) compile of the engine plus the query-latency suite — the compile alone exceeds 60 s on a cold cache, and the LDBC SF0.1
  answer gate adds a 19 MB data load on cold runners. The compile is the
  shared-cost design of ADR-0252 (one build, several gates), not slack.
  `os-smoke`/`os-filesystem`/
  `os-surface` compile the FFI and surface crates on macOS/Windows
  toolchains; the OS-delta dedup (ci-lanes.md) is what keeps them as small
  as they are. These lanes are documented exceptions, not silent drift.

The cadenced lanes have no runtime budget — nightly and 6-hourly exist
precisely so they do not need one.
