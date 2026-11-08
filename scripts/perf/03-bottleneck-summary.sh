#!/usr/bin/env bash
# Turn a perf record into a crate-layer summary so you can see which engine
# layer owns CPU. Reads /tmp/lt-<mode>.perf (from 02-run-loadtest.sh).
# perf report's stderr stays visible (02/04/05 policy).
#
# Usage: ./scripts/perf/03-bottleneck-summary.sh [memory|disk]
set -euo pipefail
cd "$(dirname "$0")/../.."
source scripts/perf/_common.sh
mode="${1:-memory}"
data="/tmp/lt-${mode}.perf"
[[ -f "$data" ]] || { perf_skip "bottleneck summary" "missing $data; run 02-run-loadtest.sh $mode first" >&2; exit 1; }

# One perf report per view, captured whole: a failed report exits here with
# its stderr visible, and the greps below can no-match without failing the
# summary (`|| true` covers only that genuine no-match case).
self_report=$(perf report -i "$data" --stdio --no-children)
children_report=$(perf report -i "$data" --stdio --children)

echo "== top functions (self) =="
{ grep -E '^\s*[0-9]+\.[0-9]+%' <<< "$self_report" || true; } | sed -n '1,25p'

echo
echo "== share by engine layer (inclusive, from call graph) =="
{ grep -E '^\s*[0-9]+\.[0-9]+%' <<< "$children_report" || true; } \
  | { grep -oE 'relate_[a-z_]+' || true; } | sort | uniq -c | sort -rn | sed -n '1,20p' \
  | while read -r n sym; do printf "%6d  %s\n" "$n" "$sym"; done

echo
echo "== lock/wait share =="
{ grep -iE 'psynch|futex|wait|lock|fsync|fcntl|sync_all' <<< "$self_report" || true; } | sed -n '1,12p'

echo
echo "Hint: if 'commit::CommitWaiter' or futex dominates in memory mode, that is"
echo "the single-writer serialize point (ADR-0014), not a removable layer."
