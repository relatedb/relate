#!/usr/bin/env bash
# The one loadtest-invocation and artifact-message policy for the perf scripts
# (02-run-loadtest, 03-bottleneck-summary, 04-cpu-ebpf, 05-flamegraph).
# Sourced, never executed directly: sourcing only defines the functions, and
# every default resolves at its one consumption point. Per-helper contracts
# live in README.md.
#
#   perf_mode_base [memory|disk]        mode table: MODE, LT, URI, MODE_ARGS
#   perf_require_data WHAT              preflight: LT.perf must exist, else exit 1
#   perf_invocation [mode] [flags...]   resolve MODE, LT, LOADTEST_BIN/COMM/ARGS
#   perf_skip WHAT REASON               report a skipped step on stderr
#   perf_announce PATH DESCRIPTION      list a produced artifact on stdout
#   perf_pick_stage INFERNO FLAMEGRAPH  a flamegraph stage's path, or empty when
#                                       neither the binary nor the script exists
#   perf_produce FILE WHAT REASON CMD.. run CMD to FILE; announce it or skip
perf_mode_base() {
  MODE="${1:-memory}"
  case "$MODE" in
    memory) LT=/tmp/lt-memory; URI=':memory:'; MODE_ARGS=() ;;
    disk)   LT=/tmp/lt-disk;   URI="$LT";      MODE_ARGS=(--reset) ;;
    *) echo "mode must be memory|disk" >&2; exit 2 ;;
  esac
}

perf_require_data() {
  [[ -f "$LT.perf" ]] || {
    perf_skip "$1" "missing $LT.perf; run 02-run-loadtest.sh $MODE first"
    exit 1
  }
}

perf_invocation() {
  perf_mode_base "${1:-}"
  shift || true

  LOADTEST_BIN=target/profiling/fraud-loadtest
  LOADTEST_COMM=${LOADTEST_BIN##*/}
  [[ -x "$LOADTEST_BIN" ]] || {
    echo "missing $LOADTEST_BIN; run scripts/perf/01-build.sh" >&2
    exit 1
  }

  LOADTEST_ARGS=(--uri "$URI" --ops "${OPS:-6000}" --warmup "${WARMUP:-400}" --workers "${WORKERS:-8}")
  LOADTEST_ARGS+=(${MODE_ARGS[@]+"${MODE_ARGS[@]}"})
  [[ -n "${WRITE_FRACTION:-}" ]] && LOADTEST_ARGS+=(--write-fraction "$WRITE_FRACTION")
  LOADTEST_ARGS+=("$@")
}

perf_skip() {
  echo "($1 skipped: $2)" >&2
}

perf_announce() {
  echo "  $1  ($2)"
}

# A flamegraph stage's path: the inferno binary when installed, else that
# stage's script from $FLAMEGRAPH_DIR, else the empty string (the chosen
# sentinel — both call sites guard on it, since a command substitution cannot
# carry a failure without `|| true` at every site). An $FLAMEGRAPH_DIR that
# lacks the script is named on stderr, so a caller's generic skip reason is not
# the only diagnostic.
perf_pick_stage() {
  if command -v "$1" >/dev/null 2>&1; then
    printf '%s' "$1"
  elif [[ -x "${FLAMEGRAPH_DIR:-}/$2" ]]; then
    printf '%s' "$FLAMEGRAPH_DIR/$2"
  elif [[ -n "${FLAMEGRAPH_DIR:-}" ]]; then
    echo "FLAMEGRAPH_DIR=$FLAMEGRAPH_DIR has no executable $2" >&2
  fi
}

# The one shape for a conditional artifact: run CMD with stdout captured to a
# sibling FILE.tmp (its stderr stays visible), then, when CMD succeeded and
# wrote something, move FILE.tmp into place and announce FILE as WHAT. A skip
# reports WHAT with the cause (CMD failed / CMD wrote nothing) and leaves
# neither FILE nor FILE.tmp behind — including a stale FILE from a previous
# run. Returns 0 when FILE was produced, 1 when it was skipped; the caller
# decides whether a skip is tolerable (`|| true`) or fatal (`|| exit 1`). CMD
# is a command or a shell function, so a producer pipeline is passed as a
# function name and the pipeline status is read from CMD's own exit.
perf_produce() {
  local file="$1" what="$2" reason="$3"
  shift 3
  local tmp="$file.tmp" cause
  if ! "$@" >"$tmp"; then
    cause="the producer exited non-zero"
  elif [[ ! -s "$tmp" ]]; then
    cause="the producer wrote nothing"
  else
    mv "$tmp" "$file"
    perf_announce "$file" "$what"
    return 0
  fi
  # A skip drops the temp and any stale FILE from a previous run.
  rm -f "$tmp" "$file"
  perf_skip "$what" "$reason: $cause"
  return 1
}
