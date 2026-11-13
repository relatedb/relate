#!/usr/bin/env bash
# The one loadtest-invocation and artifact-message policy for the perf scripts
# (02-run-loadtest, 03-bottleneck-summary, 04-cpu-ebpf, 05-flamegraph).
#
# perf_mode_base [memory|disk] — the one mode table every script goes
# through: MODE (default memory; an unknown mode exits 2 here), LT, the
# /tmp/lt-<mode> artifact base each artifact path extends, URI (the store's
# address — the disk store IS LT) and MODE_ARGS (the mode's loadtest flags;
# disk's --reset makes the loadtest reset that store itself, through the
# storage layer's own reset policy). Only perf_invocation acts on
# URI/MODE_ARGS; 03/05 read LT alone, so a mistyped mode fails loudly there
# instead of silently naming a /tmp/lt-<typo> file no run ever wrote.
#
# perf_require_data WHAT — the one preflight for a script that reads the
# loadtest's perf data: LT.perf must exist, else WHAT is reported skipped and
# the script exits 1.
#
# perf_invocation [memory|disk] [extra loadtest flags...]
#   sets MODE, LT, LOADTEST_BIN, LOADTEST_COMM, LOADTEST_ARGS. Resolves
#   everything a run needs — the mode (from the perf_mode_base table), the
#   symbolized profiling build (01-build.sh) and its process name, and the
#   shared flag vector (URI, env knobs, per-mode extras, caller extras).
#   Sourced, never executed directly; sourcing only defines the functions —
#   every default (mode and the OPS/WORKERS/WARMUP/WRITE_FRACTION env knobs)
#   resolves at its one consumption point inside perf_invocation.
#
# perf_skip WHAT REASON — the one wording for a step that produced nothing:
#   (WHAT skipped: REASON)
# A skip is a diagnostic, so it goes to stderr and every call site lands on
# the same stream; the produced rows (perf_announce) are the listing on
# stdout. Tool stderr is never masked, so a failed step's reason is visible
# above it.
#
# perf_announce PATH DESCRIPTION — the one wording for a produced artifact,
#   printed as an indented "PATH  (DESCRIPTION)" row under a "wrote:" listing.
#
# pick_stage INFERNO FLAMEGRAPH — the one availability policy for a flamegraph
#   stage: the inferno binary when it is installed, else that stage's script
#   from $FLAMEGRAPH_DIR (empty when neither is available, so callers guard on
#   the empty string — a command substitution cannot carry a failure without
#   `|| true` at every call site). An $FLAMEGRAPH_DIR that lacks the script is
#   named on stderr, so the caller's generic skip reason is not the only
#   diagnostic. 02's folded-stacks producer and 05's renderer both resolve
#   their stages through it.
#
# perf_produce FILE WHAT REASON CMD... — the one shape for a conditional
#   artifact: run CMD with stdout captured to FILE (its stderr stays visible),
#   then announce FILE as WHAT when CMD succeeded and wrote something, else
#   report WHAT skipped with REASON and leave no FILE behind. Returns 0 when
#   FILE was produced, 1 when it was skipped — the caller decides whether a
#   skip is tolerable (a best-effort artifact: `|| true`) or fatal
#   (`|| exit 1`). CMD is a command or a shell function, so a producer
#   pipeline is passed as a function name and the pipeline status is read from
#   CMD's own exit.
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

pick_stage() {
  if command -v "$1" >/dev/null 2>&1; then
    printf '%s' "$1"
  elif [[ -x "${FLAMEGRAPH_DIR:-}/$2" ]]; then
    printf '%s' "$FLAMEGRAPH_DIR/$2"
  elif [[ -n "${FLAMEGRAPH_DIR:-}" ]]; then
    echo "FLAMEGRAPH_DIR=$FLAMEGRAPH_DIR has no executable $2" >&2
  fi
}

perf_produce() {
  local file="$1" what="$2" reason="$3"
  shift 3
  if "$@" >"$file" && [[ -s "$file" ]]; then
    perf_announce "$file" "$what"
    return 0
  fi
  # The redirect above created FILE before CMD ran, so a skip leaves it
  # truncated (or half-written): drop it, rather than leave a bogus artifact
  # behind a "skipped" line.
  rm -f "$file"
  perf_skip "$what" "$reason"
  return 1
}
