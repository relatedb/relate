# Loadtest profiling on Linux

Profile scripts for the fraud/identity load tests. Run in order. All writes go to `/tmp`.

| Script | What it does | Needs |
| ------ | ------------ | ----- |
| `00-prereq.sh` | verify perf / toolchain / `perf_event_paranoid` | — |
| `00-prereq.sh --ebpf` | also verify bpftrace for CPU-level metrics | — |
| `01-build.sh` | build `fraud-loadtest`/`identity-loadtest` with debug symbols | cargo |
| `02-run-loadtest.sh [memory\|disk]` | `perf record` a run; writes `*.perf`, `*.out`, `*.report`, `*.folded` | perf |
| `03-bottleneck-summary.sh [mode]` | crate-layer + lock/wait share from a `.perf` | perf |
| `04-cpu-ebpf.sh [mode]` | `perf stat` IPC/cache/branch + optional bpftrace off-CPU/futex | perf, sudo, bpftrace(optional) |
| `05-flamegraph.sh [mode]` | render `/tmp/lt-<mode>.svg` | inferno or FlameGraph |

## Sourced helpers (`_common.sh`)

Every script sources `scripts/perf/_common.sh`, the one home of the
loadtest-invocation and artifact-message policy. Its header carries only the
signature list; the per-helper contracts live here:

- `perf_mode_base [memory|disk]` — the one mode table every script goes
  through: sets `MODE` (default memory; an unknown mode exits 2 here), `LT`
  (the `/tmp/lt-<mode>` artifact base each artifact path extends), `URI` (the
  store's address — the disk store IS `LT`) and `MODE_ARGS` (the mode's
  loadtest flags; disk's `--reset` makes the loadtest reset that store itself,
  through the storage layer's own reset policy). Only `perf_invocation` acts
  on `URI`/`MODE_ARGS`; 03/05 read `LT` alone, so a mistyped mode fails loudly
  there instead of naming a `/tmp/lt-<typo>` file no run ever wrote.
- `perf_require_data WHAT` — the one preflight for a script that reads the
  loadtest's perf data: `LT.perf` must exist, else `WHAT` is reported skipped
  and the script exits 1.
- `perf_invocation [memory|disk] [extra loadtest flags...]` — sets `MODE`,
  `LT`, `LOADTEST_BIN`, `LOADTEST_COMM`, `LOADTEST_ARGS`: the mode (from the
  `perf_mode_base` table), the symbolized profiling build (`01-build.sh`) and
  its process name, and the shared flag vector (`URI`, env knobs, per-mode
  extras, caller extras). Every default (mode and the
  `OPS`/`WORKERS`/`WARMUP`/`WRITE_FRACTION` env knobs) resolves at its one
  consumption point inside it.
- `perf_skip WHAT REASON` — the one wording for a step that produced nothing:
  `(WHAT skipped: REASON)`, on stderr. A skip is a diagnostic, so every call
  site lands on the same stream; the produced rows (`perf_announce`) are the
  listing on stdout, and tool stderr is never masked, so a failed step's reason
  is visible above it.
- `perf_announce PATH DESCRIPTION` — the one wording for a produced artifact,
  printed as an indented `PATH  (DESCRIPTION)` row under a `wrote:` listing.
- `perf_pick_stage INFERNO FLAMEGRAPH` — the one availability policy for a
  flamegraph stage: the inferno binary when it is installed, else that stage's
  script from `$FLAMEGRAPH_DIR`, else the empty string (the chosen sentinel —
  both call sites guard on it, since a command substitution cannot carry a
  failure without `|| true` at every call site). An `$FLAMEGRAPH_DIR` that
  lacks the script is named on stderr, so the caller's generic skip reason is
  not the only diagnostic. 02's folded-stacks producer and 05's renderer both
  resolve their stages through it.
- `perf_produce FILE WHAT REASON CMD...` — the one shape for a conditional
  artifact: run `CMD` with stdout captured to a sibling `FILE.tmp` (its stderr
  stays visible), then, when `CMD` succeeded and wrote something, move
  `FILE.tmp` into place and announce `FILE` as `WHAT`. A skip reports `WHAT`
  with the cause (`CMD` failed / `CMD` wrote nothing) and leaves neither
  `FILE` nor `FILE.tmp` behind — including a stale `FILE` from a previous run.
  Returns 0 when `FILE` was produced, 1 when it was skipped; the caller decides
  whether a skip is tolerable (`|| true`) or fatal (`|| exit 1`). `CMD` is a
  command or a shell function, so a producer pipeline is passed as a function
  name and the pipeline status is read from `CMD`'s own exit.

## Rules

- **Profile `memory`, not `disk`, for code-level bottlenecks.** A disk run is
  ~97% `fsync` (group commit works; ~26 commits/fsync at 50 workers). The
  engine's CPU path is invisible underneath.
- Build with `cargo build --profile profiling -p relate-datasets --bins` (`01-build.sh` does this; ADR-0244's symbolized-release profile, binaries in `target/profiling/`).
  Without symbols `perf` prints hex addresses.
- Knobs: `OPS`, `WARMUP`, `WORKERS`, `WRITE_FRACTION` env vars.

## Reading the summary

`03-bottleneck-summary.sh` prints self-time by function and an inclusive
share grouped by crate. For removal-first work, look for:

- a function with high **self** time that does pure data movement
  (materialization, cloning, a per-key interner lookup) — candidates to delete;
- `CommitWaiter` / futex high in **memory** mode — the single-writer
  serialize point (ADR-0014), a design boundary, not a removable layer.

The loadtest also emits commit-timing histograms to stderr with
`RELATE_COMMIT_TIMING=1` (parse+plan, overlay execute, wal_lock wait,
encode+write, `sync_all`, apply_redo), which is the ground truth for where
commit time goes on a disk run.
