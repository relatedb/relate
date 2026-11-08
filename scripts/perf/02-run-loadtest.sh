#!/usr/bin/env bash
# Run the fraud loadtest under perf and produce a report + folded stacks.
#
# Usage:
#   ./scripts/perf/02-run-loadtest.sh [memory|disk] [extra loadtest flags]
#
# memory  --uri :memory: (CPU-path profiling; the one to use for code-level
#           bottleneck hunting. Disk is ~97% fsync and hides engine cost.)
# disk    --uri /tmp/lt-disk (durability-path profiling; mostly fsync).
#
# Output:
#   /tmp/lt-<mode>.perf      raw perf.data (binary)
#   /tmp/lt-<mode>.out       loadtest stdout (the Throughput section greps it)
#   /tmp/lt-<mode>.report    perf report text
#   /tmp/lt-<mode>.folded    folded stacks (inferno / flamegraph.pl input)
set -euo pipefail
cd "$(dirname "$0")/../.."
source scripts/perf/_common.sh

perf_invocation "$@"

lt="/tmp/lt-${MODE}"  # one base for every artifact path derived below
data="$lt.perf"
echo "Profiling ($MODE): ${LOADTEST_ARGS[*]}"
# The loadtest's stdout (its throughput line) is captured, not leaked to the
# terminal — the Throughput section below greps it here, not the symbol
# profile, which never contains it.
perf record -F 997 --call-graph dwarf -o "$data" -- "$LOADTEST_BIN" "${LOADTEST_ARGS[@]}" > "$lt.out"

# perf report's stderr stays visible: a failed report is named as skipped
# by the echo below instead of a listing row. `sed -n '1,60p'` (not `head`)
# consumes the whole stream, so the pipeline status is honest under pipefail
# and the report block takes the folded block's exact decide-at-production
# shape.
report_ok=0
if perf report -i "$data" --stdio --no-children | sed -n '1,60p' > "$lt.report" && [[ -s "$lt.report" ]]; then
  report_ok=1
else
  perf_skip "perf report" "it produced no output for $lt.report; see error above"
fi
# Folded stacks need inferno; without it the flamegraph step is skipped
# loudly rather than half-done.
# The folded-stacks outcome is decided exactly once, here, before any
# consumer: folded_ok=1 only when the pipeline succeeded and the file is
# non-empty (the if-condition evaluates the pipeline status natively under
# pipefail — no status variable to re-derive). Both artifact blocks share
# this decide-at-production shape.
folded_ok=0
if command -v inferno-collapse-perf >/dev/null 2>&1; then
  if perf script -i "$data" | inferno-collapse-perf > "$lt.folded" && [[ -s "$lt.folded" ]]; then
    folded_ok=1
  else
    perf_skip "folded stacks" "perf script | inferno-collapse-perf produced no $lt.folded; see error above"
  fi
else
  perf_skip "folded stacks" "install inferno for $lt.folded"
fi

echo
echo "wrote:"
perf_announce "$data" "perf record data"
perf_announce "$lt.out" "loadtest stdout"
(( report_ok )) && perf_announce "$lt.report" "top functions"
(( folded_ok )) && perf_announce "$lt.folded" "folded stacks for a flamegraph"
echo
echo "Throughput:"
grep -m1 "throughput" "$lt.out" || echo "(no throughput line in $lt.out)"
