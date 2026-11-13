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
# Output (LT is the /tmp/lt-<mode> base):
#   LT.perf      raw perf.data (binary)
#   LT.out       loadtest stdout (the Throughput section greps it)
#   LT.report    perf report text
#   LT.folded    folded stacks (inferno / flamegraph.pl input)
set -euo pipefail
cd "$(dirname "$0")/../.."
source scripts/perf/_common.sh

perf_invocation "$@"

echo "Profiling ($MODE): ${LOADTEST_ARGS[*]}"
# The loadtest's stdout (its throughput line) is captured, not leaked to the
# terminal — the Throughput section below greps it here, not the symbol
# profile, which never contains it.
perf record -F 997 --call-graph dwarf -o "$LT.perf" -- "$LOADTEST_BIN" "${LOADTEST_ARGS[@]}" > "$LT.out"

# The two conditional artifacts, each a producer perf_produce runs under its
# own if — so a failed pipeline is a reported skip, never a set -e abort.
# Both are best-effort (`|| true`): the run's own artifacts are already
# written, so a skip is reported and the listing continues.
# `sed -n '1,60p'` (not `head`) consumes the whole stream, so the report
# block's pipeline status is honest under pipefail.
collapser=$(pick_stage inferno-collapse-perf stackcollapse-perf.pl)
produce_report() { perf report -i "$LT.perf" --stdio --no-children | sed -n '1,60p'; }
produce_folded() { perf script -i "$LT.perf" | "$collapser"; }

echo
echo "wrote:"
perf_announce "$LT.perf" "perf record data"
perf_announce "$LT.out" "loadtest stdout"
# perf report's stderr stays visible: a failed report is named as skipped
# instead of a listing row.
perf_produce "$LT.report" "top functions" \
  "perf report produced no output for $LT.report; see error above" produce_report || true
# Folded stacks need a collapser; without one the flamegraph step is skipped
# loudly rather than half-done.
if [[ -n "$collapser" ]]; then
  perf_produce "$LT.folded" "folded stacks for a flamegraph" \
    "perf script | $collapser produced no $LT.folded; see error above" produce_folded || true
else
  perf_skip "folded stacks" "install inferno or set FLAMEGRAPH_DIR for $LT.folded"
fi
echo
echo "Throughput:"
grep -m1 "throughput" "$LT.out" || echo "(no throughput line in $LT.out)"
