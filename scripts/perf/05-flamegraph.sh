#!/usr/bin/env bash
# Render a flamegraph SVG from /tmp/lt-<mode>.perf.
# Prefers inferno (cargo install inferno); falls back to brendangregg/FlameGraph
# (git clone https://github.com/brendangregg/FlameGraph, set FLAMEGRAPH_DIR).
# perf script's stderr stays visible (02/03/04 policy).
set -euo pipefail
cd "$(dirname "$0")/../.."
source scripts/perf/_common.sh
mode="${1:-memory}"
data="/tmp/lt-${mode}.perf"
out="/tmp/lt-${mode}.svg"
[[ -f "$data" ]] || { perf_skip "flamegraph" "missing $data; run 02-run-loadtest.sh $mode first" >&2; exit 1; }

if command -v inferno-flamegraph >/dev/null 2>&1; then
  perf script -i "$data" | inferno-collapse-perf | inferno-flamegraph > "$out"
elif command -v inferno-collapse-perf >/dev/null 2>&1 && [[ -n "${FLAMEGRAPH_DIR:-}" ]]; then
  perf script -i "$data" | inferno-collapse-perf \
    | "$FLAMEGRAPH_DIR/flamegraph.pl" > "$out"
elif [[ -n "${FLAMEGRAPH_DIR:-}" ]]; then
  perf script -i "$data" \
    | "$FLAMEGRAPH_DIR/stackcollapse-perf.pl" \
    | "$FLAMEGRAPH_DIR/flamegraph.pl" > "$out"
else
  perf_skip "flamegraph" "install inferno (cargo install inferno) or set FLAMEGRAPH_DIR to a FlameGraph checkout" >&2
  exit 1
fi
perf_announce "$out" "flamegraph SVG"
