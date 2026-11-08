#!/usr/bin/env bash
# The one loadtest-invocation and artifact-message policy for the perf scripts
# (02-run-loadtest, 03-bottleneck-summary, 04-cpu-ebpf, 05-flamegraph). perf_invocation resolves everything a run needs — mode
# (memory|disk, default memory; disk cleans its store), the symbolized
# profiling build (01-build.sh) and its process name, and the shared flag
# vector (URI, env knobs, per-mode extras, caller extras). Sourced, never
# executed directly; sourcing only defines the function — every default
# (mode and the OPS/WORKERS/WARMUP/WRITE_FRACTION env knobs) resolves at its
# one consumption point inside perf_invocation.
#
# perf_skip WHAT REASON — the one wording for a step that produced nothing:
#   (WHAT skipped: REASON)
# Tool stderr is never masked, so a failed step's reason is visible above it.
#
# perf_announce PATH DESCRIPTION — the one wording for a produced artifact,
#   printed as an indented "PATH  (DESCRIPTION)" row under a "wrote:" listing.
#
# perf_invocation [memory|disk] [extra loadtest flags...]
#   sets MODE, LOADTEST_BIN, LOADTEST_COMM, LOADTEST_ARGS
perf_invocation() {
  local uri
  local -a mode_args=()
  MODE="${1:-memory}"
  case "$MODE" in
    memory) uri=':memory:' ;;
    disk)   uri=/tmp/lt-disk; rm -rf "$uri"; mode_args=(--reset) ;;
    *) echo "mode must be memory|disk" >&2; exit 2 ;;
  esac
  shift || true

  LOADTEST_BIN=target/profiling/fraud-loadtest
  LOADTEST_COMM=${LOADTEST_BIN##*/}
  [[ -x "$LOADTEST_BIN" ]] || {
    echo "missing $LOADTEST_BIN; run scripts/perf/01-build.sh" >&2
    exit 1
  }

  LOADTEST_ARGS=(--uri "$uri" --ops "${OPS:-6000}" --warmup "${WARMUP:-400}" --workers "${WORKERS:-8}")
  LOADTEST_ARGS+=(${mode_args[@]+"${mode_args[@]}"})
  [[ -n "${WRITE_FRACTION:-}" ]] && LOADTEST_ARGS+=(--write-fraction "$WRITE_FRACTION")
  LOADTEST_ARGS+=("$@")
}

perf_skip() {
  echo "($1 skipped: $2)"
}

perf_announce() {
  echo "  $1  ($2)"
}
