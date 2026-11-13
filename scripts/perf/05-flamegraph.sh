#!/usr/bin/env bash
# Render a flamegraph SVG from LT.perf (LT is the /tmp/lt-<mode> base).
# Prefers inferno (cargo install inferno); falls back to brendangregg/FlameGraph
# (git clone https://github.com/brendangregg/FlameGraph, set FLAMEGRAPH_DIR).
# perf script's stderr stays visible (02/03/04 policy).
set -euo pipefail
cd "$(dirname "$0")/../.."
source scripts/perf/_common.sh
perf_mode_base "${1:-}"
perf_require_data "flamegraph"
out="$LT.svg"

# Pick each stage on its own — inferno when it is installed, else that stage's
# FlameGraph script from $FLAMEGRAPH_DIR — then run the one pipeline. Any mix
# of the two works, including a partial inferno install.
collapser=$(pick_stage inferno-collapse-perf stackcollapse-perf.pl)
renderer=$(pick_stage inferno-flamegraph flamegraph.pl)
if [[ -z "$collapser" || -z "$renderer" ]]; then
  perf_skip "flamegraph" "install inferno (cargo install inferno) or set FLAMEGRAPH_DIR to a FlameGraph checkout"
  exit 1
fi

# The SVG is this script's whole output, so a skip is fatal, unlike 02's
# best-effort artifacts.
produce_flamegraph() { perf script -i "$LT.perf" | "$collapser" | "$renderer"; }
perf_produce "$out" "flamegraph SVG" \
  "the render produced no $out; see error above" produce_flamegraph || exit 1
